//! `doctor`: environment and configuration checks. Modifies nothing.
//!
//! The Rust port has no Python, `uv` or Worktrunk (`wt`) runtime dependency:
//! integration detection and worktree removal are done with `gix` and plain
//! `git`, so the only external binary checked here is `git` itself.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{self, Config};
use crate::error::{Error, EXIT_DEPENDENCY, EXIT_OPERATIONAL, EXIT_USAGE};
use crate::git;
use crate::proc;
use crate::sizes::realpath;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Severity of a single check, mirroring the JSON `status` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Fail,
    Warn,
    Info,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub status: CheckStatus,
    pub detail: String,
}

impl Check {
    pub fn new(
        name: impl Into<String>,
        ok: bool,
        status: CheckStatus,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            ok,
            status,
            detail: detail.into(),
        }
    }

    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, true, CheckStatus::Ok, detail)
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, false, CheckStatus::Fail, detail)
    }

    pub fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, false, CheckStatus::Warn, detail)
    }

    pub fn info(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Self {
        Self::new(name, ok, CheckStatus::Info, detail)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
    pub exit_code: i32,
}

impl DoctorReport {
    fn push(&mut self, check: Check) {
        self.checks.push(check);
    }

    /// Raise the exit code only if nothing more specific was recorded yet,
    /// so the first (most specific) failure wins.
    fn raise(&mut self, code: i32) {
        if self.exit_code == 0 {
            self.exit_code = code;
        }
    }

    pub fn failed(&self) -> bool {
        self.exit_code != 0
    }

    /// Stable machine-readable structure: `wt-janitor.doctor/1`.
    pub fn to_json(&self) -> Value {
        json!({
            "schema": "wt-janitor.doctor/1",
            "checks": self
                .checks
                .iter()
                .map(|c| json!({
                    "name": c.name,
                    "status": c.status.as_str(),
                    "detail": c.detail,
                }))
                .collect::<Vec<_>>(),
            "exit_code": self.exit_code,
        })
    }
}

/// Run every check against `config_path` and the default state location.
pub fn run_doctor(config_path: &Path) -> DoctorReport {
    run_doctor_with_state(config_path, &config::default_state_path())
}

/// Same as [`run_doctor`], with an explicit state file location (tests).
pub fn run_doctor_with_state(config_path: &Path, state_path: &Path) -> DoctorReport {
    let mut report = DoctorReport::default();

    check_git(&mut report);
    let config = check_config(&mut report, config_path);
    if let Some(config) = &config {
        for repo in &config.repos {
            check_repo(&mut report, &repo.name, &repo.path);
        }
    }
    check_state_dir(&mut report, state_path);

    report
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

fn check_git(report: &mut DoctorReport) {
    match proc::run_cmd(&["git", "--version"], None, GIT_TIMEOUT, false, true) {
        Ok(result) if result.ok() => {
            let detail = first_non_empty(&[&result.stdout, &result.stderr]);
            report.push(Check::ok("git", detail));
        }
        Ok(result) => {
            let detail = first_non_empty(&[&result.stderr, &result.stdout]);
            report.push(Check::fail(
                "git",
                if detail.is_empty() {
                    format!("git --version exited with {}", result.returncode)
                } else {
                    detail
                },
            ));
            report.raise(EXIT_DEPENDENCY);
        }
        Err(Error::Dependency(_)) => {
            report.push(Check::fail("git", "git not found on PATH"));
            report.raise(EXIT_DEPENDENCY);
        }
        Err(err) => {
            report.push(Check::fail("git", format!("could not run git: {err}")));
            report.raise(EXIT_DEPENDENCY);
        }
    }
}

fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

fn check_config(report: &mut DoctorReport, config_path: &Path) -> Option<Config> {
    match config::load_config(config_path) {
        Ok(config) => {
            report.push(Check::ok(
                "configuration",
                config_path.display().to_string(),
            ));
            if config.repos.is_empty() {
                report.push(Check::warn(
                    "configuration",
                    "no [[repos]] configured — nothing to manage",
                ));
            }
            Some(config)
        }
        Err(err) => {
            report.push(Check::fail("configuration", err.to_string()));
            report.raise(EXIT_USAGE);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// repositories
// ---------------------------------------------------------------------------

fn check_repo(report: &mut DoctorReport, name: &str, path: &Path) {
    let label = format!("repo {name}");
    if !path.is_dir() {
        report.push(Check::fail(
            &label,
            format!("path does not exist: {}", path.display()),
        ));
        report.raise(EXIT_OPERATIONAL);
        return;
    }
    let repo = match git::open_repo(path) {
        Ok(repo) => repo,
        Err(_) => {
            report.push(Check::fail(
                &label,
                format!("not a Git repository: {}", path.display()),
            ));
            report.raise(EXIT_OPERATIONAL);
            return;
        }
    };

    let is_main = git::is_main_worktree(&repo);
    let mut detail = realpath(path).display().to_string();
    if !is_main {
        detail.push_str(" (warning: not the main worktree)");
    }
    report.push(Check::new(
        &label,
        true,
        if is_main {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        detail,
    ));

    match git::worktree_list_porcelain(path) {
        Ok(entries) => report.push(Check::ok(
            format!("{label} worktree list"),
            format!("git worktree list works ({} entries)", entries.len()),
        )),
        Err(err) => {
            report.push(Check::fail(
                format!("{label} worktree list"),
                err.to_string(),
            ));
            report.raise(EXIT_OPERATIONAL);
        }
    }

    match git::default_branch(&repo) {
        Some(branch) => report.push(Check::ok(format!("{label} default branch"), branch)),
        None => report.push(Check::warn(
            format!("{label} default branch"),
            "could not detect (integration checks will be unknown)",
        )),
    }
}

// ---------------------------------------------------------------------------
// state directory
// ---------------------------------------------------------------------------

fn check_state_dir(report: &mut DoctorReport, state_path: &Path) {
    let parent: PathBuf = state_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    match probe_writable(&parent) {
        Ok(()) => report.push(Check::ok("state directory", parent.display().to_string())),
        Err(err) => {
            report.push(Check::fail(
                "state directory",
                format!("not writable: {err}"),
            ));
            report.raise(EXIT_OPERATIONAL);
        }
    }
}

fn probe_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe")?;
    std::fs::remove_file(&probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_payload_pins_the_schema() {
        let report = DoctorReport {
            checks: vec![Check::ok("git", "git version 2.49.0")],
            exit_code: 0,
        };
        let payload = report.to_json();
        assert_eq!(payload["schema"], "wt-janitor.doctor/1");
        assert_eq!(payload["checks"][0]["name"], "git");
        assert_eq!(payload["checks"][0]["status"], "ok");
        assert_eq!(payload["exit_code"], 0);
    }

    #[test]
    fn missing_config_reports_usage_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let report = run_doctor_with_state(
            &dir.path().join("nope.toml"),
            &dir.path().join("state/state.json"),
        );
        let config = report
            .checks
            .iter()
            .find(|c| c.name == "configuration")
            .expect("configuration check present");
        assert_eq!(config.status, CheckStatus::Fail);
        assert_eq!(report.exit_code, EXIT_USAGE);
        // Git and the state directory are still checked.
        assert!(report.checks.iter().any(|c| c.name == "state directory"));
    }

    #[test]
    fn empty_repo_list_warns_but_does_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "inactive_days = 7\n").unwrap();
        let report = run_doctor_with_state(&config_path, &dir.path().join("state/state.json"));
        assert_eq!(report.exit_code, 0);
        assert!(report
            .checks
            .iter()
            .any(|c| c.name == "configuration" && c.status == CheckStatus::Warn));
    }

    #[test]
    fn missing_repo_path_is_an_operational_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "inactive_days = 7\n\n[[repos]]\nname = \"gone\"\npath = \"{}\"\n",
                dir.path().join("missing").display()
            ),
        )
        .unwrap();
        let report = run_doctor_with_state(&config_path, &dir.path().join("state/state.json"));
        assert_eq!(report.exit_code, EXIT_OPERATIONAL);
        let repo = report
            .checks
            .iter()
            .find(|c| c.name == "repo gone")
            .expect("repo check present");
        assert_eq!(repo.status, CheckStatus::Fail);
        assert!(repo.detail.starts_with("path does not exist:"));
    }
}
