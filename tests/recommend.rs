use wt_janitor::discovery::recommend;
use wt_janitor::models::{
    DepDir, Inactivity, Integration, SizeInfo, WorktreeRecord, BLOCKED_DETACHED, BLOCKED_DIRTY,
    BLOCKED_LOCKED, KEEP_ACTIVE, MISSING, PURGE_DEPS, REMOVE_MERGED, REVIEW_STALE, UNKNOWN,
};

fn record() -> WorktreeRecord {
    WorktreeRecord {
        repo: "repo".into(),
        repo_path: "/repo".into(),
        path: "/repo-worktree".into(),
        branch: Some("feature/x".into()),
        head_sha: Some("abc123".into()),
        short_sha: Some("abc123".into()),
        commit_message: Some("work".into()),
        commit_ts: Some(999_900.0),
        is_main: false,
        is_current: false,
        locked: false,
        detached: false,
        missing: false,
        dirty: false,
        dirty_kinds: Vec::new(),
        default_branch: Some("main".into()),
        main_ahead: Some(1),
        main_behind: Some(0),
        remote_name: Some("origin".into()),
        remote_ahead: Some(0),
        remote_behind: Some(0),
        integration: Integration::not_integrated(),
        in_progress_op: None,
        worktree_state_note: None,
        inactivity: Inactivity {
            seconds: 100.0,
            source: "last commit".into(),
            incomplete: false,
            just_discovered: false,
        },
        inactive: false,
        sizes: SizeInfo::default(),
        action: String::new(),
        action_detail: String::new(),
        notes: Vec::new(),
    }
}

#[test]
fn main_and_missing_worktrees_are_handled_first() {
    let mut main = record();
    main.is_main = true;
    main.missing = true;
    main.locked = true;
    assert_eq!(recommend(&main), KEEP_ACTIVE);

    let mut missing = record();
    missing.missing = true;
    missing.locked = true;
    assert_eq!(recommend(&missing), MISSING);
}

#[test]
fn locked_and_detached_worktrees_are_blocked() {
    let mut locked = record();
    locked.locked = true;
    locked.detached = true;
    locked.integration = Integration::integrated(Some("ancestor".into()));
    assert_eq!(recommend(&locked), BLOCKED_LOCKED);

    let mut detached = record();
    detached.detached = true;
    assert_eq!(recommend(&detached), BLOCKED_DETACHED);
}

#[test]
fn an_in_progress_operation_blocks_cleanup() {
    for operation in ["merge", "rebase", "cherry-pick"] {
        let mut candidate = record();
        candidate.in_progress_op = Some(operation.into());
        candidate.integration = Integration::integrated(Some("ancestor".into()));
        assert_eq!(recommend(&candidate), BLOCKED_DIRTY);
    }
}

#[test]
fn only_clean_integrated_worktrees_with_a_default_branch_are_removed() {
    let mut clean = record();
    clean.integration = Integration::integrated(Some("ancestor".into()));
    assert_eq!(recommend(&clean), REMOVE_MERGED);

    let mut dirty = clean.clone();
    dirty.dirty = true;
    dirty.dirty_kinds.push("untracked".into());
    assert_eq!(recommend(&dirty), BLOCKED_DIRTY);

    let mut no_default = clean;
    no_default.default_branch = None;
    assert_eq!(recommend(&no_default), UNKNOWN);
}

#[test]
fn unknown_integration_never_recommends_destructive_cleanup() {
    let mut candidate = record();
    candidate.integration = Integration::unknown();
    candidate.inactive = true;
    candidate.sizes.dep_dirs.push(DepDir {
        path: "/repo-worktree/node_modules".into(),
        name: "node_modules".into(),
        size_bytes: 10,
        is_symlink: false,
    });

    assert_eq!(recommend(&candidate), UNKNOWN);
}

#[test]
fn inactive_unmerged_worktrees_only_offer_dependency_cleanup() {
    let mut with_dependencies = record();
    with_dependencies.inactive = true;
    with_dependencies.inactivity.seconds = 30.0 * 86_400.0;
    with_dependencies.sizes = SizeInfo {
        total_bytes: 10,
        complete: true,
        reclaimable_bytes: 10,
        dep_dirs: vec![DepDir {
            path: "/repo-worktree/node_modules".into(),
            name: "node_modules".into(),
            size_bytes: 10,
            is_symlink: false,
        }],
    };
    assert_eq!(recommend(&with_dependencies), PURGE_DEPS);

    let mut without_dependencies = with_dependencies;
    without_dependencies.sizes = SizeInfo::default();
    assert_eq!(recommend(&without_dependencies), REVIEW_STALE);
}

#[test]
fn recent_and_just_discovered_worktrees_are_kept() {
    let recent = record();
    assert_eq!(recommend(&recent), KEEP_ACTIVE);

    let mut discovered = record();
    discovered.inactivity = Inactivity {
        seconds: 0.0,
        source: "first seen".into(),
        incomplete: false,
        just_discovered: true,
    };
    discovered.sizes.dep_dirs.push(DepDir {
        path: "/repo-worktree/node_modules".into(),
        name: "node_modules".into(),
        size_bytes: 1,
        is_symlink: false,
    });
    assert_eq!(recommend(&discovered), KEEP_ACTIVE);
}
