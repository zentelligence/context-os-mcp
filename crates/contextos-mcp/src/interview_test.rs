use std::collections::VecDeque;
use std::fs;

use tempfile::tempdir;

use super::*;
use crate::LogLevel;

/// A scripted [`Interviewer`] double: consumes fixed confirm/text answers in
/// call order, so [`run_interview`] is exercised deterministically with no
/// real terminal I/O (`specification/delivery-plan.md` Phase 13 gate).
struct ScriptedInterviewer {
    confirms: VecDeque<bool>,
    texts: VecDeque<String>,
    choices: VecDeque<usize>,
}

impl ScriptedInterviewer {
    fn new(confirms: Vec<bool>, texts: Vec<String>) -> Self {
        Self {
            confirms: confirms.into(),
            texts: texts.into(),
            choices: VecDeque::new(),
        }
    }

    /// As [`Self::new`], plus a scripted queue of [`Interviewer::choose`]
    /// selection indices, for tests exercising the multi-vault focus menu.
    fn with_choices(confirms: Vec<bool>, texts: Vec<String>, choices: Vec<usize>) -> Self {
        Self {
            confirms: confirms.into(),
            texts: texts.into(),
            choices: choices.into(),
        }
    }
}

impl Interviewer for ScriptedInterviewer {
    fn confirm(&mut self, prompt: &str, _default: bool) -> Result<bool, InterviewError> {
        self.confirms.pop_front().ok_or_else(|| script_exhausted(prompt))
    }

    fn ask(&mut self, prompt: &str) -> Result<String, InterviewError> {
        self.texts.pop_front().ok_or_else(|| script_exhausted(prompt))
    }

    fn ask_with_default(&mut self, prompt: &str, _default: &str) -> Result<String, InterviewError> {
        self.texts.pop_front().ok_or_else(|| script_exhausted(prompt))
    }

    fn choose(&mut self, prompt: &str, _options: &[&str]) -> Result<usize, InterviewError> {
        self.choices.pop_front().ok_or_else(|| script_exhausted(prompt))
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

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
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
    let download_model =
        move |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Ok(model_dir_for_closure.clone()) };
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
    assert_eq!(report.semantic_model_directory.as_deref(), Some(model_dir.path()));
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
    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
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
fn run_interview_reports_host_running_instead_of_failing_the_whole_interview() -> Result<(), Box<dyn std::error::Error>>
{
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
    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
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

/// Every test past this point loads a `config.toml` that already has
/// vault(s) configured, exercising the reload behaviour rather than the
/// fresh-install path the tests above cover.
fn write_existing_config(path: &std::path::Path, contents: &str) -> Result<(), std::io::Error> {
    fs::write(path, contents)
}

fn no_network_environment<'a>(
    config_path: PathBuf,
    detector: &'a NeverRunning,
    download_model: &'a dyn Fn(&Path) -> Result<PathBuf, ModelCliError>,
    resolve_host_path: &'a dyn Fn() -> Result<HostPathResolution, HostPathError>,
) -> InterviewEnvironment<'a> {
    InterviewEnvironment {
        config_path,
        model_cache_dir: PathBuf::new(),
        download_model,
        resolve_host_path,
        process_detector: detector,
        wait_tick: &|| {},
        server_command: "/usr/local/bin/contextos".to_owned(),
    }
}

#[test]
fn run_interview_prefills_a_single_existing_vault_and_accepts_every_default() -> Result<(), Box<dyn std::error::Error>>
{
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!("[[vault]]\npath = {:?}\nname = \"mine\"\n", vault_dir.path()),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            true,  // "Keep mine managed ...?"
            false, // "Enable semantic search for mine?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec![
            "mine".to_owned(),                      // "Vault name:" default accepted verbatim
            vault_dir.path().display().to_string(), // "Vault path ...:" default accepted verbatim
        ],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.edited_vaults, vec!["mine".to_owned()]);
    assert!(report.added_vaults.is_empty());
    let written = fs::read_to_string(&config_path)?;
    let config = Config::try_from(written.as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].path, vault_dir.path());
    Ok(())
}

#[test]
fn run_interview_prefills_a_single_existing_vault_and_renames_it() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let vault_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!("[[vault]]\npath = {:?}\nname = \"mine\"\n", vault_dir.path()),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::new(
        vec![
            true,  // "Keep renamed managed ...?"
            false, // "Enable semantic search for renamed?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec!["renamed".to_owned(), vault_dir.path().display().to_string()],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.edited_vaults, vec!["renamed".to_owned()]);
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.vaults[0].name.as_deref(), Some("renamed"));
    Ok(())
}

#[test]
fn run_interview_multi_vault_focus_edits_general_server_settings() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "[[vault]]\npath = {:?}\nname = \"first\"\n[[vault]]\npath = {:?}\nname = \"second\"\n",
            first_dir.path(),
            second_dir.path()
        ),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::with_choices(
        vec![
            false, // "Focus on something else?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec![
            "stdio, http".to_owned(), // "Transports ...:"
            "debug".to_owned(),       // "Log level ...:"
            String::new(),            // "Log file path ...:"
        ],
        vec![0], // "What would you like to focus on?" -> General (server) settings
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert!(report.server_settings_changed);
    assert!(report.edited_vaults.is_empty());
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.transports.len(), 2);
    Ok(())
}

#[test]
fn run_interview_general_server_settings_focus_lowercases_a_mixed_case_log_level()
-> Result<(), Box<dyn std::error::Error>> {
    // Regression: `LogLevel`'s TOML deserialisation only accepts its exact
    // lowercase form (`config.rs`'s `#[serde(rename_all = "lowercase")]`),
    // so an operator typing "Debug" at the log-level prompt must not abort
    // the whole interview (and every edit already accepted this run) with
    // an unrecognised-value error.
    let config_dir = tempdir()?;
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "[[vault]]\npath = {:?}\nname = \"first\"\n[[vault]]\npath = {:?}\nname = \"second\"\n",
            first_dir.path(),
            second_dir.path()
        ),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::with_choices(
        vec![
            false, // "Focus on something else?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec![
            "stdio".to_owned(), // "Transports ...:"
            "Debug".to_owned(), // "Log level ...:" mixed case
            String::new(),      // "Log file path ...:"
        ],
        vec![0], // "What would you like to focus on?" -> General (server) settings
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert!(report.server_settings_changed);
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.server.log_level, LogLevel::Debug);
    Ok(())
}

#[test]
fn run_interview_multi_vault_focus_edits_all_vaults_in_turn() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "[[vault]]\npath = {:?}\nname = \"first\"\n[[vault]]\npath = {:?}\nname = \"second\"\n",
            first_dir.path(),
            second_dir.path()
        ),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::with_choices(
        vec![
            true, false, // vault "first": managed?, semantic?
            true, false, // vault "second": managed?, semantic?
            false, // "Focus on something else?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec![
            "first".to_owned(),
            first_dir.path().display().to_string(),
            "second".to_owned(),
            second_dir.path().display().to_string(),
        ],
        vec![1], // "What would you like to focus on?" -> All vaults
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.edited_vaults, vec!["first".to_owned(), "second".to_owned()]);
    Ok(())
}

#[test]
fn run_interview_multi_vault_focus_removes_a_specific_vault() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let config_path = config_dir.path().join("config.toml");
    write_existing_config(
        &config_path,
        &format!(
            "[[vault]]\npath = {:?}\nname = \"first\"\n[[vault]]\npath = {:?}\nname = \"second\"\n",
            first_dir.path(),
            second_dir.path()
        ),
    )?;

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let resolve_host_path =
        || -> Result<HostPathResolution, HostPathError> { Err(HostPathError::HomeDirectoryUnavailable) };
    let detector = NeverRunning;
    let environment = no_network_environment(config_path.clone(), &detector, &download_model, &resolve_host_path);

    let mut interviewer = ScriptedInterviewer::with_choices(
        vec![
            false, // "Focus on something else?"
            false, // "Add a new vault?"
            false, // "Register this server with Claude Desktop now?"
        ],
        vec![],
        vec![
            2, // "What would you like to focus on?" -> A specific vault
            1, // "Which vault?" -> index 1 ("second")
            1, // "What would you like to do with second?" -> Remove
        ],
    );

    let report = run_interview(&mut interviewer, &environment)?;

    assert_eq!(report.removed_vaults, vec!["second".to_owned()]);
    let config = Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].name.as_deref(), Some("first"));
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

    let download_model = |_cache_dir: &Path| -> Result<PathBuf, ModelCliError> { Err(ModelCliError::NoRequiredFiles) };
    let host_path_for_closure = host_path.clone();
    let resolve_host_path = move || -> Result<HostPathResolution, HostPathError> {
        Ok(HostPathResolution::Found(host_path_for_closure.clone()))
    };
    let detector = RunningThenNotRunning { calls: Cell::new(0) };
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
    assert!(wait_ticks.get() > 0, "the wait loop must have polled at least once");

    Ok(())
}
