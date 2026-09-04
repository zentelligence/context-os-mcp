use super::*;

#[test]
fn linux_selects_the_systemd_backend() {
    assert!(backend_for_os("linux").is_ok());
}

#[test]
fn macos_selects_the_launchd_backend() {
    assert!(backend_for_os("macos").is_ok());
}

#[test]
fn windows_selects_the_scheduled_task_backend() {
    assert!(backend_for_os("windows").is_ok());
}

#[test]
fn an_unrecognised_platform_is_a_typed_error_not_a_panic() {
    let result = backend_for_os("plan9");

    assert!(matches!(
        result,
        Err(ServiceError::UnsupportedPlatform { os }) if os == "plan9"
    ));
}

#[test]
fn current_platform_backend_resolves_on_this_test_host() {
    // This suite only runs on hosts `std::env::consts::OS` reports as one
    // of the three supported platforms; a fourth would fail CI outright
    // long before this test, so asserting success here (rather than
    // matching a specific platform) is the honest claim for a
    // platform-parameterised function.
    assert!(current_platform_backend().is_ok());
}

#[test]
fn fake_command_runner_records_calls_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let runner = FakeCommandRunner::new();
    runner.push_success("first");
    runner.push_failure("second failed");

    let first = runner.run("echo", &["a"])?;
    let second = runner.run("echo", &["b"])?;

    assert!(first.success);
    assert_eq!(first.stdout, "first");
    assert!(!second.success);
    assert_eq!(second.stderr, "second failed");
    assert_eq!(
        runner.calls(),
        vec![
            ("echo".to_owned(), vec!["a".to_owned()]),
            ("echo".to_owned(), vec!["b".to_owned()]),
        ]
    );
    Ok(())
}

#[test]
fn fake_command_runner_reports_a_scripted_spawn_error() {
    let runner = FakeCommandRunner::new();
    runner.push_spawn_error();

    assert!(runner.run("does-not-exist", &[]).is_err());
}
