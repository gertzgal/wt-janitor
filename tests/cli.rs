mod common;

use std::fs;
use std::process::Command;

use tempfile::tempdir;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wt-janitor"))
}

#[test]
fn init_never_overwrites() {
    let temp = tempdir().unwrap();
    let config = temp.path().join("config.toml");
    let first = binary()
        .args(["init", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(first.status.success());
    fs::write(&config, "sentinel").unwrap();
    let second = binary()
        .args(["init", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(second.status.success());
    assert_eq!(fs::read_to_string(config).unwrap(), "sentinel");
}

#[test]
fn doctor_json_reports_configuration_exit_two() {
    let temp = tempdir().unwrap();
    let output = binary()
        .args(["doctor", "--json", "--config"])
        .arg(temp.path().join("missing.toml"))
        .env("WT_JANITOR_STATE", temp.path().join("state.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "wt-janitor.doctor/1");
    assert_eq!(value["exit_code"], 2);
}

#[test]
fn scan_json_and_unknown_repo_have_stable_behavior() {
    let temp = tempdir().unwrap();
    let repo = common::init_repo(&temp.path().join("repo"));
    let config = temp.path().join("config.toml");
    fs::write(
        &config,
        format!("[[repos]]\nname = \"demo\"\npath = {:?}\n", repo),
    )
    .unwrap();
    let output = binary()
        .args(["scan", "--json", "--config"])
        .arg(&config)
        .env("WT_JANITOR_STATE", temp.path().join("state.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "wt-janitor.scan/1");

    let unknown = binary()
        .args(["scan", "--repo", "missing", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

#[test]
fn clean_deps_is_a_dry_run_by_default() {
    let temp = tempdir().unwrap();
    let repo = common::init_repo(&temp.path().join("repo"));
    let config = temp.path().join("config.toml");
    fs::write(
        &config,
        format!("[[repos]]\nname = \"demo\"\npath = {:?}\n", repo),
    )
    .unwrap();
    let output = binary()
        .args(["clean", "deps", "--config"])
        .arg(&config)
        .env("WT_JANITOR_STATE", temp.path().join("state.json"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("DRY RUN — no files or worktrees will be removed."));
    assert!(stdout.contains("Re-run with --apply to execute this plan."));
}
