use std::path::Path;

use wt_janitor::git::porcelain::parse_porcelain;
use wt_janitor::sizes::realpath;

#[test]
fn parses_multiple_nul_delimited_worktrees() {
    let input = concat!(
        "worktree /repo\0",
        "HEAD 0123456789abcdef\0",
        "branch refs/heads/main\0",
        "worktree /tmp/feature worktree\0",
        "HEAD fedcba9876543210\0",
        "branch refs/heads/feature/with-slash\0",
    );

    let entries = parse_porcelain(input).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, realpath(Path::new("/repo")));
    assert_eq!(entries[0].head.as_deref(), Some("0123456789abcdef"));
    assert_eq!(entries[0].branch.as_deref(), Some("main"));
    assert_eq!(
        entries[1].path,
        realpath(Path::new("/tmp/feature worktree"))
    );
    assert_eq!(entries[1].branch.as_deref(), Some("feature/with-slash"));
}

#[test]
fn parses_detached_bare_locked_and_prunable_flags() {
    let input = concat!(
        "worktree /tmp/detached\0HEAD abc\0detached\0",
        "worktree /tmp/bare\0bare\0",
        "worktree /tmp/locked\0HEAD def\0branch refs/heads/locked\0",
        "locked reason with spaces\0prunable stale administrative data\0",
    );

    let entries = parse_porcelain(input).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(entries[0].detached);
    assert!(entries[0].branch.is_none());
    assert!(entries[1].bare);
    assert!(entries[2].locked);
    assert_eq!(
        entries[2].locked_reason.as_deref(),
        Some("reason with spaces")
    );
    assert!(entries[2].prunable);
    assert_eq!(
        entries[2].prunable_reason.as_deref(),
        Some("stale administrative data")
    );
}

#[test]
fn ignores_forward_compatible_unknown_fields() {
    let input = concat!(
        "worktree /tmp/wt\0",
        "HEAD abc\0",
        "branch refs/heads/topic\0",
        "future-field arbitrary value\0",
        "another-future-flag\0",
    );

    let entries = parse_porcelain(input).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].branch.as_deref(), Some("topic"));
}

#[test]
fn ignores_incomplete_records_without_a_worktree_path() {
    let entries = parse_porcelain("HEAD abc\0branch refs/heads/orphan\0").unwrap();
    assert!(entries.is_empty());
}

#[test]
fn an_empty_value_is_distinct_from_a_present_reason() {
    let entries = parse_porcelain("worktree /tmp/wt\0HEAD abc\0locked\0prunable\0").unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].locked);
    assert_eq!(entries[0].locked_reason, None);
    assert!(entries[0].prunable);
    assert_eq!(entries[0].prunable_reason, None);
}

#[test]
fn final_record_does_not_require_a_trailing_nul() {
    let entries =
        parse_porcelain("worktree /tmp/wt\0HEAD abc\0branch refs/heads/no-trailing-nul").unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].branch.as_deref(), Some("no-trailing-nul"));
}
