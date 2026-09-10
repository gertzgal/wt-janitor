mod common;

use std::fs;
use std::path::Path;

use serde_json::json;
use wt_janitor::cleanup;
use wt_janitor::config::RepoConfig;
use wt_janitor::deps;
use wt_janitor::discovery;
use wt_janitor::error::Error;
use wt_janitor::models::{
    DepDir, BLOCKED_DETACHED, BLOCKED_DIRTY, BLOCKED_LOCKED, KEEP_ACTIVE, MISSING, PURGE_DEPS,
    REMOVE_MERGED,
};
use wt_janitor::state::{atomic_write_json, load_state, record_touch};

use common::{add_worktree, by_branch, commit_in, git, git_unchecked, Scenario};

#[test]
fn discovers_the_full_classification_matrix_in_real_git_worktrees() {
    let scenario = Scenario::new();
    let now = discovery::now_secs();
    let state = json!({
        "worktrees": {
            scenario.stale.to_string_lossy().as_ref(): {
                "first_seen": now - 30.0 * 86_400.0
            }
        }
    });

    let scan = scenario.scan(&state, false, now);

    let main = by_branch(&scan, Some("main"));
    assert!(main.is_main);
    assert_eq!(main.action, KEEP_ACTIVE);

    let normal = by_branch(&scan, Some("feature/normal"));
    assert_eq!(normal.integration.state, "integrated");
    assert!(!normal.dirty);
    assert_eq!(normal.action, REMOVE_MERGED);

    let squash = by_branch(&scan, Some("feature/squash"));
    assert_eq!(squash.integration.state, "integrated");
    assert!(squash.integration.reason.is_some());
    assert_eq!(squash.action, REMOVE_MERGED);

    let stale = by_branch(&scan, Some("feature/stale"));
    assert_eq!(stale.integration.state, "not-integrated");
    assert!(stale.inactive);
    assert_eq!(stale.action, PURGE_DEPS);
    let dependency_names = stale
        .sizes
        .dep_dirs
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect::<Vec<_>>();
    assert!(dependency_names.contains(&"node_modules"));
    assert!(dependency_names.contains(&".venv"));

    let locked = by_branch(&scan, Some("feature/locked"));
    assert!(locked.locked);
    assert_eq!(locked.action, BLOCKED_LOCKED);

    let detached = by_branch(&scan, None);
    assert!(detached.detached);
    assert_eq!(detached.action, BLOCKED_DETACHED);

    let dirty = by_branch(&scan, Some("feature/dirty"));
    assert!(dirty.dirty);
    assert!(dirty.dirty_kinds.iter().any(|kind| kind == "untracked"));
    assert_eq!(dirty.action, BLOCKED_DIRTY);

    assert!(scan
        .worktrees
        .iter()
        .any(|record| record.path.contains("dir with spaces")));

    let active = by_branch(&scan, Some("feature/active"));
    assert_eq!(active.integration.state, "not-integrated");
    assert!(!active.inactive);
    assert_eq!(active.action, KEEP_ACTIVE);
}

#[test]
fn first_seen_shields_old_work_and_touch_reactivates_established_work() {
    let scenario = Scenario::new();
    let now = discovery::now_secs();
    let stale_path = scenario.stale.to_string_lossy().into_owned();

    let just_seen = json!({
        "worktrees": {stale_path.as_str(): {"first_seen": now}}
    });
    let first_scan = scenario.scan(&just_seen, false, now);
    let stale = by_branch(&first_scan, Some("feature/stale"));
    assert_eq!(stale.inactivity.source, "first seen");
    assert!(stale.inactivity.just_discovered);
    assert!(!stale.inactive);
    assert_eq!(stale.action, KEEP_ACTIVE);

    let mut established = json!({
        "worktrees": {stale_path.as_str(): {"first_seen": now - 30.0 * 86_400.0}}
    });
    assert!(
        by_branch(
            &scenario.scan(&established, false, now),
            Some("feature/stale")
        )
        .inactive
    );

    record_touch(&mut established, &stale_path, now);
    let touched_scan = scenario.scan(&established, false, now);
    let touched = by_branch(&touched_scan, Some("feature/stale"));
    assert_eq!(touched.inactivity.source, "explicit touch");
    assert!(!touched.inactive);
    assert_eq!(touched.action, KEEP_ACTIVE);
}

#[test]
fn reports_missing_worktrees_without_dropping_other_results() {
    let scenario = Scenario::new();
    let missing_path = scenario.active.canonicalize().unwrap();
    fs::remove_dir_all(&scenario.active).unwrap();

    let scan = scenario.scan(&json!({}), false, discovery::now_secs());
    let missing = scan
        .worktrees
        .iter()
        .find(|record| record.path == missing_path.to_string_lossy())
        .expect("deleted but registered worktree should remain visible");
    assert!(missing.missing);
    assert_eq!(missing.action, MISSING);
    assert!(scan.worktrees.len() > 1);
}

#[test]
fn fetch_failures_mark_results_stale_but_preserve_local_discovery() {
    let scenario = Scenario::new();
    git(
        &scenario.repo,
        [
            "remote",
            "add",
            "origin",
            "/definitely/missing/wt-janitor.git",
        ],
    );

    let scan = scenario.scan(&json!({}), true, discovery::now_secs());
    assert_eq!(scan.fetch_ok, Some(false));
    assert!(scan.fetch_stale);
    assert!(scan.errors.iter().any(|error| error.contains("stale")));
    assert!(!scan.worktrees.is_empty());
}

#[test]
fn a_merge_in_progress_blocks_removal() {
    let scenario = Scenario::new();
    let conflict = add_worktree(
        &scenario.repo,
        &scenario.repo.parent().unwrap().join("conflict-wt"),
        Some("feature/conflict"),
        false,
    );
    commit_in(
        &conflict,
        "conflict.txt",
        "conflict side",
        "worktree version\n",
    );
    commit_in(
        &scenario.repo,
        "conflict.txt",
        "main side",
        "main version\n",
    );
    let merge = git_unchecked(&conflict, ["merge", "main"]);
    assert!(
        !merge.status.success(),
        "test setup must produce a conflict"
    );

    let scan = scenario.scan(&json!({}), false, discovery::now_secs());
    let record = by_branch(&scan, Some("feature/conflict"));
    assert_eq!(record.in_progress_op.as_deref(), Some("merge"));
    assert_eq!(record.action, BLOCKED_DIRTY);
}

#[test]
fn dependency_cleanup_plans_first_then_deletes_only_allowlisted_directories() {
    let scenario = Scenario::new();
    let now = discovery::now_secs();
    let state = json!({
        "worktrees": {
            scenario.stale.to_string_lossy().as_ref(): {
                "first_seen": now - 30.0 * 86_400.0
            }
        }
    });
    let state_dir = tempfile::tempdir().unwrap();
    let state_path = state_dir.path().join("state.json");
    atomic_write_json(&state_path, &state).unwrap();
    let config = scenario.config();
    let repos = [&config.repos[0]];

    let plans = cleanup::plan_deps(&config, &repos, &load_state(&state_path), None);
    assert_eq!(plans.len(), 1);
    assert!(plans[0].entries.len() >= 2);
    assert!(scenario.stale.join("node_modules").is_dir());
    assert!(scenario.stale.join(".venv").is_dir());

    let applied = cleanup::apply_deps(&config, &repos, &state_path, None);
    assert!(applied[0].deleted_bytes > 0);
    assert!(!scenario.stale.join("node_modules").exists());
    assert!(!scenario.stale.join("pkg/node_modules").exists());
    assert!(!scenario.stale.join(".venv").exists());
    assert!(scenario.stale.join("src.txt").is_file());
    assert!(scenario.stale.join("file.txt").is_file());
    assert!(scenario.stale.join("pkg").is_dir());
}

#[test]
fn merged_cleanup_removes_integrated_worktrees_and_preserves_blocked_work() {
    let scenario = Scenario::new();
    let mut config = scenario.config();
    config.prune_min_age_days = Some(0);
    let repos = [&config.repos[0]];

    let plans = cleanup::plan_merged(&config, &repos, &json!({}), false);
    let candidate_branches = plans[0]
        .candidates
        .iter()
        .filter_map(|entry| entry.branch.as_deref())
        .collect::<Vec<_>>();
    assert!(candidate_branches.contains(&"feature/normal"));
    assert!(candidate_branches.contains(&"feature/squash"));
    assert!(plans[0]
        .protected
        .values()
        .any(|reason| reason.contains("locked")));
    assert!(plans[0]
        .protected
        .values()
        .any(|reason| reason.contains("detached")));

    let applied = cleanup::apply_merged(&config, &repos, &json!({}), false);
    assert!(applied[0].removed >= 2, "errors: {:?}", applied[0].errors);
    assert!(!scenario.normal.exists());
    assert!(!scenario.squash.exists());
    assert!(scenario.locked.exists());
    assert!(scenario.detached.exists());
    assert!(scenario.dirty.exists());
    assert!(scenario.stale.exists());
    assert!(scenario.active.exists());
}

#[cfg(unix)]
#[test]
fn symlink_cleanup_unlinks_only_the_link_and_traversal_is_refused() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    let outside = temp.path().join("outside-store");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("payload.txt"), "precious\n").unwrap();

    let link = worktree.join("node_modules");
    symlink(&outside, &link).unwrap();
    let candidate = DepDir {
        path: link.to_string_lossy().into_owned(),
        name: "node_modules".into(),
        size_bytes: 0,
        is_symlink: true,
    };
    let removed = deps::delete_target(&candidate, &worktree, &repo).unwrap();
    assert_eq!(removed, link);
    assert!(!link.exists());
    assert!(outside.join("payload.txt").is_file());

    let escape = DepDir {
        path: worktree
            .join("../outside-store")
            .to_string_lossy()
            .into_owned(),
        name: "outside-store".into(),
        size_bytes: 0,
        is_symlink: false,
    };
    let error = deps::validate_target(&escape, &worktree, &repo).unwrap_err();
    assert!(matches!(error, Error::Safety(_)));

    for protected in [Path::new("/"), repo.as_path(), worktree.as_path()] {
        let protected_candidate = DepDir {
            path: protected.to_string_lossy().into_owned(),
            name: "protected".into(),
            size_bytes: 0,
            is_symlink: false,
        };
        assert!(matches!(
            deps::validate_target(&protected_candidate, &worktree, &repo),
            Err(Error::Safety(_))
        ));
    }
}

#[test]
fn a_broken_repo_does_not_prevent_other_cleanup_plans() {
    let scenario = Scenario::new();
    let mut config = scenario.config();
    let broken_path = scenario.repo.parent().unwrap().join("does-not-exist");
    config.repos.insert(
        0,
        RepoConfig {
            name: "broken".into(),
            raw_path: broken_path.to_string_lossy().into_owned(),
            path: broken_path,
        },
    );
    let repos = config.repos.iter().collect::<Vec<_>>();

    let plans = cleanup::plan_deps(&config, &repos, &json!({}), None);
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0].repo_name, "broken");
    assert_eq!(plans[0].errors.len(), 1);
    assert_eq!(plans[1].repo_name, "test");
}
