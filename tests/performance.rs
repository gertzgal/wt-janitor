mod common;

use std::fs;
use std::time::Instant;

use serde_json::json;
use wt_janitor::{cleanup, discovery};

use common::{add_worktree, commit_in, init_repo};

/// Manual regression harness for the production failure mode: many files in a
/// worktree that cannot possibly be removed. Everything lives in a TempDir;
/// the cleanup invocation is dry-run only.
#[test]
#[ignore = "manual performance harness"]
fn merged_plan_skips_irrelevant_filesystem_walks() {
    let temp = tempfile::tempdir().unwrap();
    let repo = init_repo(&temp.path().join("repo"));
    fs::write(repo.join(".gitignore"), "generated/\n").unwrap();
    common::git(&repo, ["add", ".gitignore"]);
    common::git(&repo, ["commit", "-qm", "ignore generated fixture"]);

    let worktree = add_worktree(
        &repo,
        &temp.path().join("feature"),
        Some("feature/not-integrated"),
        false,
    );
    commit_in(&worktree, "feature.txt", "feature work", "not merged\n");

    let file_count = std::env::var("WT_JANITOR_PERF_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50_000);
    for index in 0..file_count {
        let directory = worktree
            .join("generated")
            .join(format!("{:03}", index / 1_000));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(format!("{index}.dat")), b"fixture").unwrap();
    }

    let config = wt_janitor::config::Config {
        inactive_days: 7,
        dependency_dirs: vec!["node_modules".into()],
        prune_min_age_days: Some(0),
        repos: vec![wt_janitor::config::RepoConfig {
            name: "fixture".into(),
            raw_path: repo.to_string_lossy().into_owned(),
            path: repo.clone(),
        }],
        path: temp.path().join("config.toml"),
    };
    let repos = [&config.repos[0]];

    let full_started = Instant::now();
    let full = discovery::discover_repo(
        &config.repos[0],
        &json!({}),
        config.inactive_days,
        &config.dependency_dirs,
        false,
        discovery::now_secs(),
    )
    .unwrap();
    let full_elapsed = full_started.elapsed();

    let merged_started = Instant::now();
    let plans = cleanup::plan_merged(&config, &repos, &json!({}), false);
    let merged_elapsed = merged_started.elapsed();

    assert_eq!(plans.len(), 1);
    assert!(plans[0].candidates.is_empty());
    assert!(full.worktrees.iter().any(|record| {
        record.branch.as_deref() == Some("feature/not-integrated") && record.sizes.total_bytes > 0
    }));
    eprintln!(
        "files={file_count} full={full_elapsed:?} merged={merged_elapsed:?} speedup={:.1}x",
        full_elapsed.as_secs_f64() / merged_elapsed.as_secs_f64()
    );
}
