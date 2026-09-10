#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use wt_janitor::config::{Config, RepoConfig};
use wt_janitor::discovery::{self, RepoScan};
use wt_janitor::models::WorktreeRecord;

pub const OLD_TIMESTAMP: f64 = 1_072_915_200.0; // 2004-01-01T00:00:00Z

pub fn git<I, S>(cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let output = git_command(cwd)
        .args(args.iter().map(AsRef::as_ref))
        .output()
        .expect("git must be installed for the integration test suite");
    assert!(
        output.status.success(),
        "git command failed in {} (status {:?}):\nstdout: {}\nstderr: {}",
        cwd.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

pub fn git_unchecked<I, S>(cwd: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_command(cwd)
        .args(args)
        .output()
        .expect("git must be installed for the integration test suite")
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        // Fixtures must not inherit the developer's Git configuration. Commit
        // signing in particular makes the suite fail intermittently, because
        // parallel tests overwhelm the signing agent.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "wt-janitor test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "wt-janitor test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
    command
}

pub fn init_repo(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    git(path, ["init", "-q", "-b", "main"]);
    fs::write(path.join("file.txt"), "hello\n").unwrap();
    git(path, ["add", "."]);
    git(path, ["commit", "-qm", "init"]);
    path.to_path_buf()
}

pub fn add_worktree(repo: &Path, path: &Path, branch: Option<&str>, detached: bool) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut args = vec!["worktree", "add", "-q"];
    if detached {
        args.push("--detach");
    } else if let Some(branch) = branch {
        args.extend(["-b", branch]);
    }
    let path_string = path.to_string_lossy().into_owned();
    args.push(&path_string);
    git(repo, args);
    path.to_path_buf()
}

pub fn commit_in(path: &Path, filename: &str, message: &str, content: &str) {
    let destination = path.join(filename);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&destination, content).unwrap();
    git(path, ["add", filename]);
    git(path, ["commit", "-qm", message]);
}

fn backdate_head(worktree: &Path) {
    let output = git_command(worktree)
        .args(["commit", "-q", "--amend", "--no-edit"])
        .env("GIT_AUTHOR_DATE", "2004-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2004-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to backdate commit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn backdate_source_files(root: &Path) {
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(OLD_TIMESTAMP as u64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let file_type = entry.file_type().unwrap();
            if name == ".git"
                || name == "node_modules"
                || name == ".venv"
                || name == "venv"
                || name == "dist"
                || name == "build"
            {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(entry.path())
                    .unwrap();
                file.set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
        }
    }
}

#[derive(Debug)]
pub struct Scenario {
    _temp: TempDir,
    pub repo: PathBuf,
    pub normal: PathBuf,
    pub squash: PathBuf,
    pub stale: PathBuf,
    pub locked: PathBuf,
    pub detached: PathBuf,
    pub dirty: PathBuf,
    pub spaces: PathBuf,
    pub active: PathBuf,
}

impl Scenario {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let repo = init_repo(&base.join("main-repo"));

        let normal = add_worktree(
            &repo,
            &base.join("merged-wt"),
            Some("feature/normal"),
            false,
        );
        commit_in(&normal, "f2.txt", "feature work", "change\n");
        git(
            &repo,
            [
                "merge",
                "-q",
                "--no-ff",
                "feature/normal",
                "-m",
                "merge normal",
            ],
        );

        let squash = add_worktree(
            &repo,
            &base.join("squash-wt"),
            Some("feature/squash"),
            false,
        );
        commit_in(&squash, "s.txt", "squash work", "squash\n");
        git(&repo, ["merge", "-q", "--squash", "feature/squash"]);
        git(&repo, ["commit", "-qm", "squash feature/squash"]);

        let stale = add_worktree(&repo, &base.join("stale-wt"), Some("feature/stale"), false);
        fs::create_dir_all(stale.join("node_modules/pkg")).unwrap();
        fs::write(stale.join("node_modules/pkg/index.js"), "x").unwrap();
        fs::create_dir_all(stale.join("pkg/node_modules")).unwrap();
        fs::write(stale.join("pkg/node_modules/x.js"), "y").unwrap();
        fs::create_dir_all(stale.join(".venv/bin")).unwrap();
        fs::write(stale.join(".venv/bin/activate"), "z").unwrap();
        fs::write(stale.join("src.txt"), "source").unwrap();
        git(&stale, ["add", "-A"]);
        git(&stale, ["commit", "-qm", "stale feature"]);
        backdate_head(&stale);
        backdate_source_files(&stale);

        let locked = add_worktree(
            &repo,
            &base.join("locked-wt"),
            Some("feature/locked"),
            false,
        );
        commit_in(&locked, "l.txt", "locked work", "locked\n");
        git(&repo, ["merge", "-q", "feature/locked"]);
        let locked_string = locked.to_string_lossy().into_owned();
        git(&repo, ["worktree", "lock", locked_string.as_str()]);

        let detached = add_worktree(&repo, &base.join("detached-wt"), None, true);

        let dirty = add_worktree(&repo, &base.join("dirty-wt"), Some("feature/dirty"), false);
        commit_in(&dirty, "d.txt", "dirty work", "dirty\n");
        git(
            &repo,
            [
                "merge",
                "-q",
                "--no-ff",
                "feature/dirty",
                "-m",
                "merge dirty",
            ],
        );
        fs::write(dirty.join("scratch.txt"), "untracked\n").unwrap();

        let spaces = add_worktree(
            &repo,
            &base.join("dir with spaces/space-wt"),
            Some("feature/spaces"),
            false,
        );
        commit_in(&spaces, "sp.txt", "spaces work", "spaces\n");
        git(&repo, ["merge", "-q", "feature/spaces"]);

        let active = add_worktree(
            &repo,
            &base.join("active-wt"),
            Some("feature/active"),
            false,
        );
        commit_in(&active, "a.txt", "active work", "active\n");

        Self {
            _temp: temp,
            repo,
            normal,
            squash,
            stale,
            locked,
            detached,
            dirty,
            spaces,
            active,
        }
    }

    pub fn repo_config(&self) -> RepoConfig {
        RepoConfig {
            name: "test".into(),
            raw_path: self.repo.to_string_lossy().into_owned(),
            path: self.repo.clone(),
        }
    }

    pub fn config(&self) -> Config {
        Config {
            inactive_days: 7,
            dependency_dirs: vec!["node_modules".into(), ".venv".into()],
            prune_min_age_days: None,
            repos: vec![self.repo_config()],
            path: self.repo.join("test-config.toml"),
        }
    }

    pub fn scan(&self, state: &serde_json::Value, do_fetch: bool, now: f64) -> RepoScan {
        discovery::discover_repo(
            &self.repo_config(),
            state,
            7,
            &["node_modules".into(), ".venv".into()],
            do_fetch,
            now,
        )
        .unwrap()
    }
}

pub fn by_branch<'a>(scan: &'a RepoScan, branch: Option<&str>) -> &'a WorktreeRecord {
    scan.worktrees
        .iter()
        .find(|record| record.branch.as_deref() == branch)
        .unwrap_or_else(|| panic!("branch {branch:?} not found in scan"))
}
