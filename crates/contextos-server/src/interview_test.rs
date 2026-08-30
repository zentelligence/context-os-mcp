use std::collections::VecDeque;
use std::fs;

use tempfile::tempdir;

use super::*;

/// A scripted [`Interviewer`] double: consumes fixed confirm/text answers in
/// call order, so [`run_interview`] is exercised deterministically with no
/// real terminal I/O (`specification/delivery-plan.md` Phase 13 gate).
struct ScriptedInterviewer {
    confirms: VecDeque<bool>,
    texts: VecDeque<String>,
}

impl ScriptedInterviewer {
    fn new(confirms: Vec<bool>, texts: Vec<String>) -> Self {
        Self {
            confirms: confirms.into(),
            texts: texts.into(),
        }
    }
}

impl Interviewer for ScriptedInterviewer {
    fn confirm(&mut self, prompt: &str, _default: bool) -> Result<bool, InterviewError> {
        self.confirms
            .pop_front()
            .ok_or_else(|| script_exhausted(prompt))
    }

    fn ask(&mut self, prompt: &str) -> Result<String, InterviewError> {
        self.texts
            .pop_front()
            .ok_or_else(|| script_exhausted(prompt))
    }
}

/// A typed failure for a [`ScriptedInterviewer`] that ran out of scripted
/// answers, reported through the same `InterviewError::Prompt` variant a
/// real cancelled `inquire` prompt would surface, so an unexpected extra
/// call fails the test with a clear `?`-propagated message instead of
/// panicking.
fn script_exhausted(prompt: &str) -> InterviewError {
    InterviewError::Prompt(inquire::InquireError::Custom(
        format!("scripted interviewer has no answer queued for prompt: {prompt:?}").into(),
    ))
}

/// A [`DetectsRunningProcesses`] test double that never reports Claude
/// Desktop as running, so `register` proceeds without a real process scan.
struct NeverRunning;

impl DetectsRunningProcesses for NeverRunning {
    fn is_running(&self, _name_needle: &str) -> bool {
        false
    }
}

#[test]
fn parse_manual_path_strips_matching_surrounding_double_quotes() {
    // The real-world case: Windows Explorer's "Copy as path" wraps the
    // clipboard text in double quotes, which then landed literally inside
    // the path and made it resolve as nonexistent.
    assert_eq!(
        parse_manual_path("\"C:\\Users\\pj\\Claude\\claude_desktop_config.json\""),
        PathBuf::from("C:\\Users\\pj\\Claude\\claude_desktop_config.json")
    );
}

#[test]
fn parse_manual_path_strips_matching_surrounding_single_quotes() {
    assert_eq!(
        parse_manual_path("'/home/pj/config.toml'"),
        PathBuf::from("/home/pj/config.toml")
    );
}

#[test]
fn parse_manual_path_leaves_an_unquoted_path_unchanged() {
    assert_eq!(
        parse_manual_path("/home/pj/config.toml"),
        PathBuf::from("/home/pj/config.toml")
    );
}

#[test]
fn parse_manual_path_leaves_mismatched_quotes_unchanged() {
    assert_eq!(
        parse_manual_path("\"/home/pj/config.toml"),
        PathBuf::from("\"/home/pj/config.toml")
    );
}

#[test]
fn run_interview_strips_quotes_from_a_manually_entered_host_path_once_discovery_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let host_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    let host_path = host_dir.path().join("claude_desktop_config.json");

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> {
        Err(ModelCliError::NoRequiredFiles)
    };
    let resolve_host_path = || -> Result<HostPathResolution, HostPathError> {
        Ok(HostPathResolution::NotFound {
            reason: "no install found".to_owned(),
        })
    };
    let detector = NeverRunning;
    let environment = InterviewEnvironment {
        config_path: config_path.clone(),
        model_cache_dir: tempdir()?.path().to_path_buf(),
        download_model: &download_model,
        resolve_host_path: &resolve_host_path,
        process_detector: &detector,
        wait_tick: &|| {},
        server_command: "/usr/local/bin/contextos".to_owned(),
    };

    // Pasted as a quoted string, mirroring "Copy as path" from a file
    // manager.
    let quoted_host_path = format!("\"{}\"", host_path.display());
    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // "Add another vault?"
            false, // "Enable semantic search ...?"
            true,  // "Register this server with Claude Desktop now?"
        ],
        vec![
            "mine".to_owned(),
            vault_dir.path().display().to_string(),
            quoted_host_path,
        ],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(
        report.host_registration,
        HostRegistrationOutcome::Registered {
            host_path: host_path.clone()
        }
    );
    assert!(host_path.exists());

    Ok(())
}

#[test]
fn run_interview_completes_the_full_happy_path_with_no_terminal_or_network_access()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let host_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    let host_path = host_dir.path().join("claude_desktop_config.json");

    let model_dir_for_closure = model_dir.path().to_path_buf();
    let download_model = move |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> {
        Ok(model_dir_for_closure.clone())
    };
    let host_path_for_closure = host_path.clone();
    let resolve_host_path = move || -> Result<HostPathResolution, HostPathError> {
        Ok(HostPathResolution::Found(host_path_for_closure.clone()))
    };
    let detector = NeverRunning;
    let environment = InterviewEnvironment {
        config_path: config_path.clone(),
        model_cache_dir: tempdir()?.path().to_path_buf(),
        download_model: &download_model,
        resolve_host_path: &resolve_host_path,
        process_detector: &detector,
        wait_tick: &|| {},
        server_command: "/usr/local/bin/contextos".to_owned(),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // "Add another vault?"
            true,  // "Enable semantic search ...?"
            true,  // "Download the local embedding model now?"
            true,  // "Register this server with Claude Desktop now?"
        ],
        vec!["mine".to_owned(), vault_dir.path().display().to_string()],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.added_vaults, vec!["mine".to_owned()]);
    assert_eq!(
        report.semantic_model_directory.as_deref(),
        Some(model_dir.path())
    );
    assert_eq!(
        report.host_registration,
        HostRegistrationOutcome::Registered {
            host_path: host_path.clone()
        }
    );

    let written = fs::read_to_string(&config_path)?;
    let config = Config::try_from(written.as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert!(config.vaults[0].search.semantic);
    assert_eq!(
        config.vaults[0].search.embedding.model_directory.as_deref(),
        Some(model_dir.path())
    );

    let registered: serde_json::Value = serde_json::from_str(&fs::read_to_string(&host_path)?)?;
    assert_eq!(
        registered["mcpServers"]["contextos"]["command"],
        "/usr/local/bin/contextos"
    );

    Ok(())
}

#[test]
fn run_interview_skips_every_optional_step_and_touches_neither_network_nor_host()
-> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");

    // Neither closure should ever run in this path: `run_interview` must
    // stop asking before reaching either one once both optional steps are
    // declined. Returning a typed error (rather than panicking) means a
    // regression that does call one of these surfaces as a normal `?`-
    // propagated test failure, not a panic, matching this crate's
    // never-panic convention.
    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> {
        Err(ModelCliError::NoRequiredFiles)
    };
    let resolve_host_path = || -> Result<HostPathResolution, HostPathError> {
        Err(HostPathError::HomeDirectoryUnavailable)
    };
    let detector = NeverRunning;
    let environment = InterviewEnvironment {
        config_path: config_path.clone(),
        model_cache_dir: tempdir()?.path().to_path_buf(),
        download_model: &download_model,
        resolve_host_path: &resolve_host_path,
        process_detector: &detector,
        wait_tick: &|| {},
        server_command: "/usr/local/bin/contextos".to_owned(),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // "Add another vault?"
            false, // "Enable semantic search ...?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec!["mine".to_owned(), vault_dir.path().display().to_string()],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.added_vaults, vec!["mine".to_owned()]);
    assert_eq!(report.semantic_model_directory, None);
    assert_eq!(report.host_registration, HostRegistrationOutcome::Skipped);

    let written = fs::read_to_string(&config_path)?;
    let config = Config::try_from(written.as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert!(!config.vaults[0].search.semantic);

    Ok(())
}

#[test]
fn run_interview_reports_host_running_instead_of_failing_the_whole_interview()
-> Result<(), Box<dyn std::error::Error>> {
    struct AlwaysRunning;
    impl DetectsRunningProcesses for AlwaysRunning {
        fn is_running(&self, _name_needle: &str) -> bool {
            true
        }
    }

    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let host_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    let host_path = host_dir.path().join("claude_desktop_config.json");

    // Must not run: the scripted answers decline semantic search, so
    // `run_interview` should never reach the download step. See the
    // sibling minimal-path test's comment for why this returns a typed
    // error instead of panicking.
    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> {
        Err(ModelCliError::NoRequiredFiles)
    };
    let host_path_for_closure = host_path.clone();
    let resolve_host_path = move || -> Result<HostPathResolution, HostPathError> {
        Ok(HostPathResolution::Found(host_path_for_closure.clone()))
    };
    let detector = AlwaysRunning;
    let environment = InterviewEnvironment {
        config_path: config_path.clone(),
        model_cache_dir: tempdir()?.path().to_path_buf(),
        download_model: &download_model,
        resolve_host_path: &resolve_host_path,
        process_detector: &detector,
        wait_tick: &|| {},
        server_command: "/usr/local/bin/contextos".to_owned(),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // "Add another vault?"
            false, // "Enable semantic search ...?"
            true,  // "Register this server with Claude Desktop now?"
            false, // "... Wait for it to close and register automatically?"
        ],
        vec!["mine".to_owned(), vault_dir.path().display().to_string()],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(
        report.host_registration,
        HostRegistrationOutcome::HostRunning {
            host_path: host_path.clone()
        }
    );
    assert!(!host_path.exists());

    Ok(())
}

#[test]
fn run_interview_waits_for_claude_desktop_to_close_then_registers_automatically()
-> Result<(), Box<dyn std::error::Error>> {
    use std::cell::Cell;

    // Reports running for its first two checks (the loop's own re-check and
    // one wait-loop poll), then not running from the third check onwards
    // (the wait loop's final poll, and `register`'s own immediately-before-
    // write re-check), so the retried registration goes through.
    struct RunningThenNotRunning {
        calls: Cell<u32>,
    }
    impl DetectsRunningProcesses for RunningThenNotRunning {
        fn is_running(&self, _name_needle: &str) -> bool {
            let call = self.calls.get();
            self.calls.set(call + 1);
            call < 2
        }
    }

    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let host_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    let host_path = host_dir.path().join("claude_desktop_config.json");

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> {
        Err(ModelCliError::NoRequiredFiles)
    };
    let host_path_for_closure = host_path.clone();
    let resolve_host_path = move || -> Result<HostPathResolution, HostPathError> {
        Ok(HostPathResolution::Found(host_path_for_closure.clone()))
    };
    let detector = RunningThenNotRunning {
        calls: Cell::new(0),
    };
    let wait_ticks = Cell::new(0u32);
    let wait_tick = || wait_ticks.set(wait_ticks.get() + 1);
    let environment = InterviewEnvironment {
        config_path: config_path.clone(),
        model_cache_dir: tempdir()?.path().to_path_buf(),
        download_model: &download_model,
        resolve_host_path: &resolve_host_path,
        process_detector: &detector,
        wait_tick: &wait_tick,
        server_command: "/usr/local/bin/contextos".to_owned(),
    };

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            false, // "Add another vault?"
            false, // "Enable semantic search ...?"
            true,  // "Register this server with Claude Desktop now?"
            true,  // "... Wait for it to close and register automatically?"
        ],
        vec!["mine".to_owned(), vault_dir.path().display().to_string()],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(
        report.host_registration,
        HostRegistrationOutcome::Registered {
            host_path: host_path.clone()
        }
    );
    assert!(host_path.exists());
    assert!(
        wait_ticks.get() > 0,
        "the wait loop must have polled at least once"
    );

    Ok(())
}
