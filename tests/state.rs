use std::fs;

use serde_json::{json, Value};
use wt_janitor::state::{
    atomic_write_json, get_worktree_state, load_state, persist_state, record_discovery,
    record_touch,
};

#[test]
fn atomic_write_creates_parent_and_leaves_valid_json_only() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state/state.json");

    atomic_write_json(&target, &json!({"worktrees": {"/x": {"first_seen": 1.0}}})).unwrap();

    let parsed: Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(parsed["worktrees"]["/x"]["first_seen"], 1.0);
    assert_eq!(
        fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .count(),
        0
    );
}

#[test]
fn atomic_write_replaces_existing_document() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");

    atomic_write_json(&target, &json!({"generation": 1})).unwrap();
    atomic_write_json(&target, &json!({"generation": 2})).unwrap();

    assert_eq!(load_state(&target), json!({"generation": 2}));
}

#[cfg(unix)]
#[test]
fn state_file_is_private_to_the_current_user() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");
    atomic_write_json(&target, &json!({})).unwrap();

    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn load_state_tolerates_missing_corrupt_and_non_object_state() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");

    assert_eq!(load_state(&target), json!({}));
    fs::write(&target, "{not json").unwrap();
    assert_eq!(load_state(&target), json!({}));
    fs::write(&target, "[]").unwrap();
    assert_eq!(load_state(&target), json!({}));
}

#[test]
fn discovery_records_first_seen_once_and_preserves_other_entries() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("state.json");

    persist_state(&target, |state| {
        state["metadata"] = json!({"keep": true});
        record_discovery(state, &["/wt/a".into(), "/wt/b".into()], 10.0);
    })
    .unwrap();
    persist_state(&target, |state| {
        record_discovery(state, &["/wt/a".into(), "/wt/c".into()], 99.0);
    })
    .unwrap();

    let state = load_state(&target);
    assert_eq!(state["metadata"]["keep"], true);
    assert_eq!(state["worktrees"]["/wt/a"]["first_seen"], 10.0);
    assert_eq!(state["worktrees"]["/wt/b"]["first_seen"], 10.0);
    assert_eq!(state["worktrees"]["/wt/c"]["first_seen"], 99.0);
}

#[test]
fn touch_updates_existing_state_and_labels_its_source() {
    let mut state = json!({});
    record_discovery(&mut state, &["/wt".into()], 1.0);
    record_touch(&mut state, "/wt", 42.0);

    let worktree = get_worktree_state(&state, "/wt").unwrap();
    assert_eq!(worktree.first_seen, 1.0);
    assert_eq!(worktree.last_used, Some(42.0));
    assert_eq!(worktree.last_used_source.as_deref(), Some("touch"));
}

#[test]
fn touch_initializes_an_unseen_worktree() {
    let mut state = json!({});
    record_touch(&mut state, "/new", 42.0);

    let worktree = get_worktree_state(&state, "/new").unwrap();
    assert_eq!(worktree.first_seen, 42.0);
    assert_eq!(worktree.last_used, Some(42.0));
}

#[test]
fn malformed_or_missing_worktree_entries_are_ignored() {
    let state = json!({
        "worktrees": {
            "/missing-first-seen": {"last_used": 2.0},
            "/wrong-type": "bad"
        }
    });

    assert!(get_worktree_state(&state, "/absent").is_none());
    assert!(get_worktree_state(&state, "/missing-first-seen").is_none());
    assert!(get_worktree_state(&state, "/wrong-type").is_none());
}
