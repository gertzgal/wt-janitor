use std::fs;
use std::time::Duration;

use wt_janitor::error::Error;
use wt_janitor::proc::{redact, run_cmd};

const SHORT_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn shell_metacharacters_remain_literal_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("must-not-exist");
    let payload = format!("safe; touch {}; marker", marker.display());

    let result = run_cmd(&["printf", "%s", &payload], None, SHORT_TIMEOUT, true, true).unwrap();

    assert_eq!(result.stdout, payload);
    assert!(!marker.exists());
    assert_eq!(result.args, ["printf", "%s", payload.as_str()]);
}

#[test]
fn empty_argument_array_is_refused() {
    let error = run_cmd(&[], None, SHORT_TIMEOUT, false, true).unwrap_err();
    assert!(error.to_string().contains("argument array"));
}

#[test]
fn cwd_with_spaces_is_passed_without_quoting_or_a_shell() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("dir with spaces");
    fs::create_dir(&cwd).unwrap();

    let result = run_cmd(&["pwd"], Some(&cwd), SHORT_TIMEOUT, true, true).unwrap();
    assert_eq!(
        result.stdout.trim(),
        cwd.canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn a_missing_binary_is_a_dependency_error() {
    let error = run_cmd(
        &["definitely-not-a-real-command-wt-janitor"],
        None,
        SHORT_TIMEOUT,
        false,
        true,
    )
    .unwrap_err();

    assert!(matches!(error, Error::Dependency(_)));
}

#[test]
fn timeout_kills_the_child_and_returns_an_operational_error() {
    let error = run_cmd(
        &["sleep", "5"],
        None,
        Duration::from_millis(100),
        false,
        true,
    )
    .unwrap_err();

    assert!(matches!(error, Error::Operational(_)));
    assert!(error.to_string().contains("timed out"));
}

#[test]
fn check_controls_nonzero_exit_handling() {
    let unchecked = run_cmd(
        &["sh", "-c", "printf problem >&2; exit 7"],
        None,
        SHORT_TIMEOUT,
        false,
        true,
    )
    .unwrap();
    assert_eq!(unchecked.returncode, 7);
    assert_eq!(unchecked.stderr, "problem");
    assert!(!unchecked.ok());

    let checked = run_cmd(
        &["sh", "-c", "printf problem >&2; exit 7"],
        None,
        SHORT_TIMEOUT,
        true,
        true,
    )
    .unwrap_err();
    assert!(checked.to_string().contains("command failed (7)"));
}

#[test]
fn drains_large_stdout_and_stderr_without_deadlocking() {
    let result = run_cmd(
        &[
            "sh",
            "-c",
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
        ],
        None,
        SHORT_TIMEOUT,
        true,
        true,
    )
    .unwrap();

    assert_eq!(result.stdout.len(), 1_048_576);
    assert_eq!(result.stderr.len(), 1_048_576);
}

#[test]
fn url_credentials_are_redacted_without_changing_safe_text() {
    assert_eq!(
        redact("fetch https://user:token123@github.com/org/repo"),
        "fetch https://***@github.com/org/repo"
    );
    assert_eq!(
        redact("https://a:first@example.com and ssh://b:second@example.net/x"),
        "https://***@example.com and ssh://***@example.net/x"
    );
    assert_eq!(redact("no secrets here"), "no secrets here");
}
