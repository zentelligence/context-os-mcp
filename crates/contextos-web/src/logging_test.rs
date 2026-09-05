use std::path::PathBuf;
use std::sync::Mutex;

use tempfile::tempdir;
use time::macros::datetime;

use super::*;

/// A deterministic [`Clock`]: advanced explicitly by the test, never by
/// wall-clock sleeps, so day-boundary and retention-age behaviour is
/// reproducible. A `Mutex`, not a `Cell`, so `&FakeClock` stays `Send +
/// Sync` (`Clock`'s own supertrait bound, needed because the real
/// `RotatingLog<SystemClock>` this mirrors is installed as part of a
/// global `tracing` subscriber).
struct FakeClock(Mutex<OffsetDateTime>);

impl FakeClock {
    fn new(start: OffsetDateTime) -> Self {
        Self(Mutex::new(start))
    }

    fn advance(&self, by: Duration) {
        let mut current = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *current += by;
    }
}

impl Clock for &FakeClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn rotated_siblings_of(path: &std::path::Path) -> Vec<PathBuf> {
    let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let prefix = format!("{file_name}.");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut siblings: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    siblings.sort();
    siblings
}

#[test]
fn a_write_under_every_threshold_never_rotates() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1024),
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    log.write_all(b"one line\n")?;
    log.write_all(b"another line\n")?;

    assert_eq!(std::fs::read_to_string(&path)?, "one line\nanother line\n");
    assert!(rotated_siblings_of(&path).is_empty());
    Ok(())
}

#[test]
fn exceeding_the_size_threshold_rotates_before_the_write_that_would_overflow_it()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(10),
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    log.write_all(b"12345")?;
    clock.advance(Duration::seconds(1));
    log.write_all(b"1234567890")?;

    let siblings = rotated_siblings_of(&path);
    assert_eq!(siblings.len(), 1, "expected exactly one rotated file: {siblings:?}");
    assert_eq!(std::fs::read_to_string(&siblings[0])?, "12345");
    assert_eq!(std::fs::read_to_string(&path)?, "1234567890");
    Ok(())
}

#[test]
fn a_utc_day_change_rotates_under_daily_rotation() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 23:59:00 UTC));
    let policy = LogRotationPolicy {
        rotate_daily: true,
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    log.write_all(b"still the 5th\n")?;
    clock.advance(Duration::minutes(2));
    log.write_all(b"now the 6th\n")?;

    let siblings = rotated_siblings_of(&path);
    assert_eq!(siblings.len(), 1, "expected exactly one rotated file: {siblings:?}");
    assert_eq!(std::fs::read_to_string(&siblings[0])?, "still the 5th\n");
    assert_eq!(std::fs::read_to_string(&path)?, "now the 6th\n");
    Ok(())
}

#[test]
fn either_trigger_rotates_when_both_are_configured() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 23:59:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1024),
        rotate_daily: true,
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    log.write_all(b"under the size limit\n")?;
    clock.advance(Duration::minutes(2));
    log.write_all(b"but the day changed\n")?;

    assert_eq!(rotated_siblings_of(&path).len(), 1);
    Ok(())
}

#[test]
fn retention_by_file_count_keeps_only_the_newest_rotations() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1),
        retention_files: Some(2),
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    for line in 0..4 {
        clock.advance(Duration::seconds(1));
        log.write_all(format!("line-{line}\n").as_bytes())?;
    }

    let siblings = rotated_siblings_of(&path);
    assert_eq!(siblings.len(), 2, "expected retention to prune down to 2: {siblings:?}");
    Ok(())
}

#[test]
fn retention_by_age_deletes_rotations_older_than_the_configured_days() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1),
        retention_days: Some(7),
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    log.write_all(b"old\n")?;
    // Every subsequent rotation prunes against the *current* clock, so the
    // first rotated file must already be older than the retention window by
    // the time a later write triggers the prune that would delete it.
    clock.advance(Duration::days(10));
    log.write_all(b"still recent\n")?;

    let siblings = rotated_siblings_of(&path);
    assert_eq!(
        siblings.len(),
        1,
        "the 10-day-old rotation should have been pruned: {siblings:?}"
    );
    assert_eq!(std::fs::read_to_string(&path)?, "still recent\n");
    Ok(())
}

#[test]
fn no_retention_configured_keeps_every_rotated_file() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1),
        ..LogRotationPolicy::default()
    };
    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;

    for line in 0..5 {
        clock.advance(Duration::seconds(1));
        log.write_all(format!("line-{line}\n").as_bytes())?;
    }

    assert_eq!(rotated_siblings_of(&path).len(), 5);
    Ok(())
}

#[test]
fn reopening_an_existing_file_preserves_its_content_and_size() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("web.log");
    std::fs::write(&path, "already here\n")?;
    let clock = FakeClock::new(datetime!(2026-09-05 10:00:00 UTC));
    let policy = LogRotationPolicy {
        max_size_bytes: Some(1024),
        ..LogRotationPolicy::default()
    };

    let mut log = RotatingLog::open(path.clone(), policy, &clock)?;
    log.write_all(b"appended\n")?;

    assert_eq!(std::fs::read_to_string(&path)?, "already here\nappended\n");
    Ok(())
}

#[test]
fn an_empty_log_file_opens_stderr_only_with_no_rotating_writer() -> Result<(), Box<dyn std::error::Error>> {
    let config = WebServerConfig::default();
    let opened = open(&config)?;
    assert!(opened.is_none());
    Ok(())
}

#[test]
fn a_configured_log_file_opens_a_rotating_writer() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let config = WebServerConfig {
        log_file: dir.path().join("web.log").to_string_lossy().into_owned(),
        log_max_size_mb: Some(10),
        ..WebServerConfig::default()
    };

    let opened = open(&config)?;

    assert!(opened.is_some());
    Ok(())
}
