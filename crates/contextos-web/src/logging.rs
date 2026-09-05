//! Wires `web.toml`'s `[server]` logging fields (`log_file`,
//! `log_max_size_mb`, `log_rotate_daily`, `log_retention_days`,
//! `log_retention_files`) into an actual rotating file destination for
//! `tracing`, closing the gap where `log_file` was accepted and displayed
//! on `/settings/` but never used: every log line went to stderr
//! regardless of its value.
//!
//! [`RotatingLog`] is a single-file, `tracing_subscriber`-compatible
//! [`Write`] destination. It rotates the live file to a timestamped sibling
//! (`{log_file}.{YYYYMMDDTHHMMSSZ}`) when either configured trigger fires
//! (`log_max_size_mb` reached, or the UTC calendar day changes under
//! `log_rotate_daily`), then prunes older rotated siblings against
//! `log_retention_days`/`log_retention_files`. Rotation and retention are
//! both keyed off an injected [`Clock`] (`architecture.md`'s "inject clocks
//! ... where determinism requires it"), never the wall clock directly, so
//! day-boundary and age-based retention behaviour is deterministically
//! testable.
//!
//! Deliberately not a general-purpose logging crate integration
//! (`tracing-appender` covers time-based rotation only; nothing on the
//! workspace's existing dependency graph covers size-based rotation or
//! count/age retention together): this is a small, fully-owned
//! implementation in the same spirit as [`crate::atomic_write`], not a new
//! external dependency pulled in for one narrow need.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime, UtcOffset};

use crate::config::WebServerConfig;

/// Injectable time source, mirroring `contextos-core::pipeline::Clock`:
/// this crate does not depend on `contextos-core` for anything beyond
/// `VaultPath`/`VaultSet` (`D-W03`), so rotation gets its own equally small
/// trait rather than an extra cross-crate dependency for one method.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Rotation and retention triggers, resolved once from [`WebServerConfig`]
/// at startup (`log_max_size_mb`'s megabyte unit converted to the bytes
/// [`RotatingLog`] actually compares against).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogRotationPolicy {
    pub max_size_bytes: Option<u64>,
    pub rotate_daily: bool,
    pub retention_days: Option<u32>,
    pub retention_files: Option<u32>,
}

impl From<&WebServerConfig> for LogRotationPolicy {
    fn from(config: &WebServerConfig) -> Self {
        Self {
            max_size_bytes: config.log_max_size_mb.map(|megabytes| megabytes * 1024 * 1024),
            rotate_daily: config.log_rotate_daily,
            retention_days: config.log_retention_days,
            retention_files: config.log_retention_files,
        }
    }
}

/// Opens `config.server.log_file` as a [`RotatingLog`], or `None` when
/// `log_file` is empty (logging goes to stderr, `main.rs`'s existing
/// behaviour, untouched). `config.server.validate_logging` already rejects
/// a rotation/retention setting paired with an empty `log_file` before this
/// is ever called, so a `Some` policy here always has somewhere to apply.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `log_file` cannot be opened
/// (for example, its parent directory does not exist): a startup error,
/// not a lazily-discovered one, matching `mcp_client`'s own connect-time
/// failure discipline.
pub fn open(config: &WebServerConfig) -> io::Result<Option<RotatingLog<SystemClock>>> {
    if config.log_file.is_empty() {
        return Ok(None);
    }
    let policy = LogRotationPolicy::from(config);
    RotatingLog::open(PathBuf::from(&config.log_file), policy, SystemClock).map(Some)
}

const ROTATED_SUFFIX: &[time::format_description::FormatItem<'_>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");

/// A single rotating log file: every [`Write`] call checks `policy`'s
/// triggers before writing, rotating the current file to a timestamped
/// sibling first when one fires, then pruning older siblings.
pub struct RotatingLog<C: Clock> {
    path: PathBuf,
    file: File,
    written_since_rotation: u64,
    current_day: time::Date,
    policy: LogRotationPolicy,
    clock: C,
}

impl<C: Clock> RotatingLog<C> {
    /// Opens (creating if absent) `path` in append mode. The rotation
    /// clock starts from `clock.now()`'s own day at open time, not any day
    /// implied by the file's existing content: a restart never triggers an
    /// immediate rotation purely because the file predates today, only the
    /// next real day-boundary crossing this process itself observes does.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when `path` cannot be opened or
    /// its metadata cannot be read.
    pub fn open(path: PathBuf, policy: LogRotationPolicy, clock: C) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written_since_rotation = file.metadata()?.len();
        let current_day = utc_now(&clock).date();
        Ok(Self {
            path,
            file,
            written_since_rotation,
            current_day,
            policy,
            clock,
        })
    }

    fn should_rotate(&self, incoming: usize) -> bool {
        if self.policy.rotate_daily && utc_now(&self.clock).date() != self.current_day {
            return true;
        }
        match self.policy.max_size_bytes {
            Some(max) => self.written_since_rotation.saturating_add(incoming as u64) > max,
            None => false,
        }
    }

    /// Renames the current file to a timestamped sibling, reopens `path` as
    /// a fresh empty file, then prunes older siblings against `policy`'s
    /// retention bounds. A pruning failure is swallowed (best-effort
    /// housekeeping, never a reason to fail the write that triggered
    /// rotation, matching the write pipeline's "downstream failures become
    /// warnings, never a rolled-back write" precedent); a rotation failure
    /// itself (the rename or reopen) is not, and propagates to the caller.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let rotated = self.rotated_sibling_path();
        std::fs::rename(&self.path, &rotated)?;
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.written_since_rotation = 0;
        self.current_day = utc_now(&self.clock).date();
        let _ = self.prune_rotated_siblings();
        Ok(())
    }

    /// A not-yet-existing `{file_name}.{timestamp}` sibling path: a numeric
    /// suffix is appended only in the (test-only in practice) case of two
    /// rotations landing on the same clock second, so concurrent rotations
    /// never clobber each other's rotated file. The timestamp is always the
    /// injected clock's own value, never the real wall clock, so retention
    /// age comparisons ([`prune_rotated_siblings`](Self::prune_rotated_siblings))
    /// stay deterministic under a fake clock in tests exactly as they do
    /// against [`SystemClock`] in production, rather than silently falling
    /// back to the filesystem's own real mtime.
    fn rotated_sibling_path(&self) -> PathBuf {
        let stamp = utc_now(&self.clock)
            .format(ROTATED_SUFFIX)
            .unwrap_or_else(|_| "0".to_owned());
        let mut candidate = sibling_with_suffix(&self.path, &stamp);
        let mut disambiguator: u32 = 1;
        while candidate.exists() {
            candidate = sibling_with_suffix(&self.path, &format!("{stamp}-{disambiguator}"));
            disambiguator += 1;
        }
        candidate
    }

    /// Every existing `{file_name}.*` sibling of `path` in its own
    /// directory that this log itself rotated (its name parses back as a
    /// [`rotated_sibling_path`](Self::rotated_sibling_path)-shaped
    /// timestamp), paired with that rotation's own clock timestamp. A
    /// sibling whose name does not parse (a foreign file, or one whose
    /// naming this log did not produce) is left alone entirely: retention
    /// only ever prunes files this log itself created, never guesses at an
    /// unrecognised one's age.
    fn rotated_siblings(&self) -> io::Result<Vec<(PathBuf, OffsetDateTime)>> {
        let Some(file_name) = self.path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            return Ok(Vec::new());
        };
        let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
        let prefix = format!("{file_name}.");
        let mut siblings = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(rotated_at) = name.strip_prefix(&prefix).and_then(parse_rotation_timestamp) {
                siblings.push((entry.path(), rotated_at));
            }
        }
        Ok(siblings)
    }

    /// Deletes rotated siblings that violate either configured retention
    /// bound: beyond the `retention_files` most recently rotated, or
    /// rotated more than `retention_days` before the current (injected)
    /// clock time. A bound left unset never deletes on that basis
    /// (`validate_logging` already requires at least one rotation trigger
    /// whenever retention is configured at all, so this only runs when it
    /// can have an effect). An individual file's delete failure is
    /// skipped, not propagated: one permission-denied sibling must not
    /// stop every other sibling's pruning.
    fn prune_rotated_siblings(&self) -> io::Result<()> {
        let mut siblings = self.rotated_siblings()?;
        if siblings.is_empty() {
            return Ok(());
        }
        siblings.sort_by_key(|(_, rotated_at)| std::cmp::Reverse(*rotated_at));

        let mut doomed: Vec<&Path> = Vec::new();
        if let Some(keep) = self.policy.retention_files {
            doomed.extend(siblings.iter().skip(keep as usize).map(|(path, _)| path.as_path()));
        }
        if let Some(days) = self.policy.retention_days {
            let cutoff = utc_now(&self.clock) - Duration::days(i64::from(days));
            doomed.extend(
                siblings
                    .iter()
                    .filter(|(_, rotated_at)| *rotated_at < cutoff)
                    .map(|(path, _)| path.as_path()),
            );
        }
        for path in doomed {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

/// Length, in bytes, of a [`ROTATED_SUFFIX`]-formatted timestamp
/// (`YYYYMMDDTHHMMSSZ`): every component is fixed-width, so this is a
/// compile-time constant, not something computed from a sample value.
const ROTATED_SUFFIX_LEN: usize = 16;

/// Recovers the clock timestamp embedded in one rotated file's own
/// dotted-suffix (the part of its name after `{file_name}.`), tolerating a
/// trailing `-N` same-second disambiguator
/// ([`rotated_sibling_path`](RotatingLog::rotated_sibling_path)) after the
/// fixed-width timestamp. `None` for anything that does not parse: a
/// foreign file sharing the prefix by coincidence, or a truncated/corrupted
/// name.
fn parse_rotation_timestamp(suffix: &str) -> Option<OffsetDateTime> {
    let stamp = suffix.get(..ROTATED_SUFFIX_LEN)?;
    let naive = PrimitiveDateTime::parse(stamp, ROTATED_SUFFIX).ok()?;
    Some(naive.assume_utc())
}

/// `clock.now()`, normalised to UTC: every rotation decision, filename
/// timestamp, and retention comparison is expressed in UTC regardless of
/// what offset a particular [`Clock`] implementation's `now()` happens to
/// return, so `"UTC calendar day"` and the retained/parsed filename
/// timestamps stay unambiguous.
fn utc_now<C: Clock>(clock: &C) -> OffsetDateTime {
    clock.now().to_offset(UtcOffset::UTC)
}

impl<C: Clock> Write for RotatingLog<C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.should_rotate(buf.len()) {
            self.rotate()?;
        }
        let written = self.file.write(buf)?;
        self.written_since_rotation += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
#[path = "logging_test.rs"]
mod tests;
