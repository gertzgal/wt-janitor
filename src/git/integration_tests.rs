use super::*;
use std::{fs, path::Path, process::Command};

struct Fixture {
    dir: tempfile::TempDir,
    repo: gix::Repository,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = gix::init(dir.path()).unwrap();
        Self { dir, repo }
    }

    fn commit(
        &self,
        parents: &[gix::ObjectId],
        files: &[(&str, &str)],
        message: &str,
    ) -> gix::ObjectId {
        let mut tree = self.repo.empty_tree().edit().unwrap();
        for (path, content) in files {
            let blob = self.repo.write_blob(content).unwrap().detach();
            tree.upsert(*path, gix::objs::tree::EntryKind::Blob, blob)
                .unwrap();
        }
        let signature = gix::actor::Signature {
            name: "Test".into(),
            email: "test@example.invalid".into(),
            time: gix::date::Time {
                seconds: 1_700_000_000,
                offset: 0,
            },
        };
        self.repo
            .write_object(gix::objs::Commit {
                tree: tree.write().unwrap().detach(),
                parents: parents.iter().copied().collect(),
                author: signature.clone(),
                committer: signature,
                encoding: None,
                message: message.into(),
                extra_headers: Vec::new(),
            })
            .unwrap()
            .detach()
    }

    fn reference(&self, name: &str, oid: gix::ObjectId) {
        self.repo
            .reference(
                name,
                oid,
                gix::refs::transaction::PreviousValue::Any,
                "test",
            )
            .unwrap();
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.dir.path())
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn integration_reasons_follow_priority() {
    let f = Fixture::new();
    let base = f.commit(&[], &[], "base");
    let empty = f.commit(&[base], &[], "empty branch");
    let branch = f.commit(&[base], &[("file", "added\n")], "branch");
    let same_tree = f.commit(&[base], &[("file", "added\n")], "squash");
    let target = f.commit(
        &[same_tree],
        &[("file", "added\n"), ("extra", "target\n")],
        "target",
    );
    let changed = f.commit(
        &[target],
        &[("file", "changed\n"), ("extra", "target\n")],
        "later edit",
    );
    for (branch, target, expected) in [
        (base, base, IntegrationReason::SameCommit),
        (base, target, IntegrationReason::Ancestor),
        (empty, base, IntegrationReason::NoAddedChanges),
        (branch, same_tree, IntegrationReason::TreesMatch),
        (branch, target, IntegrationReason::MergeAddsNothing),
        (branch, changed, IntegrationReason::PatchIdMatch),
    ] {
        assert_eq!(
            check_against(&f.repo, branch, target).unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn upstream_selection_handles_equal_ahead_behind_and_diverged_targets() {
    let f = Fixture::new();
    let base = f.commit(&[], &[], "base");
    let local = f.commit(&[base], &[("local", "local")], "local");
    let upstream = f.commit(&[base], &[("upstream", "upstream")], "upstream");
    f.git(&["config", "remote.backup.url", "."]);
    f.git(&[
        "config",
        "remote.backup.fetch",
        "+refs/heads/*:refs/remotes/custom/*",
    ]);
    f.git(&["config", "branch.main.remote", "backup"]);
    f.git(&["config", "branch.main.merge", "refs/heads/main"]);
    for (local_tip, upstream_tip, branch, expected) in [
        (local, local, local, IntegrationReason::SameCommit),
        (base, local, base, IntegrationReason::Ancestor),
        (local, base, base, IntegrationReason::Ancestor),
        (local, upstream, local, IntegrationReason::SameCommit),
        (local, upstream, upstream, IntegrationReason::SameCommit),
    ] {
        f.reference("refs/heads/main", local_tip);
        f.reference("refs/remotes/custom/main", upstream_tip);
        let repo = gix::open(f.dir.path()).unwrap();
        assert_eq!(
            super::super::internals::branch_upstream(&repo, "main")
                .unwrap()
                .0,
            "backup"
        );
        for name in ["main", "refs/heads/main"] {
            assert_eq!(
                integration_reason(&repo, branch, name).unwrap(),
                Some(expected)
            );
        }
    }
}

fn object_files(path: &Path) -> BTreeSet<std::path::PathBuf> {
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(object_files(&path));
        } else {
            files.insert(path);
        }
    }
    files
}

#[test]
fn merge_probe_keeps_new_blobs_and_trees_in_memory() {
    let f = Fixture::new();
    let base = f.commit(&[], &[("file", "one\ntwo\nthree\nfour\nfive\n")], "base");
    let branch = f.commit(
        &[base],
        &[("file", "ONE\ntwo\nthree\nfour\nfive\n")],
        "branch",
    );
    let target = f.commit(
        &[base],
        &[("file", "one\ntwo\nthree\nfour\nFIVE\n")],
        "target",
    );
    let objects = f.repo.common_dir().join("objects");
    let before = object_files(&objects);
    assert!(matches!(
        merge_probe(&f.repo, branch, target),
        MergeProbe::WouldAdd
    ));
    assert_eq!(object_files(&objects), before);
    assert_eq!(check_against(&f.repo, branch, target).unwrap(), None);
}

#[test]
fn patch_scan_accepts_500_commits_and_rejects_501_even_with_a_match() {
    let f = Fixture::new();
    let base = f.commit(&[], &[], "base");
    let branch = f.commit(&[base], &[("file", "added")], "branch");
    let mut target = f.commit(&[base], &[("file", "added")], "squash");
    for i in 1..PATCH_ID_SCAN_MAX_COMMITS {
        target = f.commit(&[target], &[("file", "changed")], &format!("target {i}"));
    }
    assert_eq!(commits_in_range(&f.repo, base, target).unwrap().len(), 500);
    assert_eq!(
        check_against(&f.repo, branch, target).unwrap(),
        Some(IntegrationReason::PatchIdMatch)
    );
    target = f.commit(&[target], &[("file", "changed")], "overflow");
    assert_eq!(commits_in_range(&f.repo, base, target).unwrap().len(), 501);
    assert_eq!(check_against(&f.repo, branch, target).unwrap(), None);
}

#[test]
fn patch_range_excludes_merged_ancestors_of_base() {
    let f = Fixture::new();
    let root = f.commit(&[], &[], "root");
    let side = f.commit(&[root], &[], "side");
    let base = f.commit(&[root, side], &[], "base");
    let target = f.commit(&[base, side], &[], "target");
    assert_eq!(
        commits_in_range(&f.repo, base, target).unwrap(),
        vec![target]
    );
}

#[test]
fn renames_are_added_changes_and_patch_paths_are_distinct() {
    let f = Fixture::new();
    let base = f.commit(&[], &[("old", "contents")], "base");
    let branch = f.commit(&[base], &[("new", "contents")], "rename");
    let other = f.commit(&[base], &[("different", "contents")], "other rename");
    assert_eq!(has_added_changes(&f.repo, branch, base), Some(true));
    assert_eq!(check_against(&f.repo, branch, base).unwrap(), None);
    assert_ne!(
        patch_id_between(&f.repo, base, branch),
        patch_id_between(&f.repo, base, other)
    );
}

#[test]
fn commit_info_decodes_committer_time() {
    let f = Fixture::new();
    let id = f.commit(&[], &[], "subject\n\nbody").to_string();
    assert_eq!(
        super::super::internals::commit_info(&f.repo, &id),
        (Some(id), Some("subject".into()), Some(1_700_000_000.0))
    );
}

#[test]
fn dirty_status_includes_hidden_untracked_staged_modified_and_deleted_files() {
    let f = Fixture::new();
    let base = f.commit(
        &[],
        &[
            (".gitignore", "ignored/\n"),
            ("modified", "old"),
            ("deleted", "old"),
        ],
        "base",
    );
    f.git(&["read-tree", "--reset", "-u", &base.to_string()]);
    f.reference("HEAD", base);
    assert!(super::super::status::dirty_kinds(f.dir.path()).is_empty());
    f.git(&["config", "status.showUntrackedFiles", "no"]);
    fs::create_dir(f.dir.path().join("ignored")).unwrap();
    fs::write(f.dir.path().join("ignored/file"), "ignored").unwrap();
    assert!(super::super::status::dirty_kinds(f.dir.path()).is_empty());
    fs::create_dir(f.dir.path().join("untracked")).unwrap();
    fs::write(f.dir.path().join("untracked/file"), "new").unwrap();
    assert_eq!(
        super::super::status::dirty_kinds(f.dir.path()),
        vec!["untracked"]
    );
    fs::write(f.dir.path().join("modified"), "changed contents").unwrap();
    fs::remove_file(f.dir.path().join("deleted")).unwrap();
    fs::write(f.dir.path().join("staged"), "new").unwrap();
    f.git(&["add", "staged"]);
    assert_eq!(
        super::super::status::dirty_kinds(f.dir.path()),
        vec!["deleted", "modified", "staged", "untracked"]
    );
}

#[test]
fn inaccessible_repository_is_not_clean() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        super::super::status::dirty_kinds(dir.path()),
        vec!["unknown"]
    );
}
