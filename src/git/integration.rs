use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use sha1::{Digest, Sha1};

use crate::error::Result;
use crate::models::Integration;

const PATCH_ID_SCAN_MAX_COMMITS: usize = 500;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationReason {
    SameCommit,
    Ancestor,
    NoAddedChanges,
    TreesMatch,
    MergeAddsNothing,
    PatchIdMatch,
}

impl IntegrationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameCommit => "same-commit",
            Self::Ancestor => "ancestor",
            Self::NoAddedChanges => "no-added-changes",
            Self::TreesMatch => "trees-match",
            Self::MergeAddsNothing => "merge-adds-nothing",
            Self::PatchIdMatch => "patch-id-match",
        }
    }
}

#[derive(Default)]
pub struct IntegrationCache {
    commit_patch_ids: Mutex<HashMap<gix::ObjectId, Arc<OnceLock<Option<String>>>>>,
}

pub fn classify(
    repo: &gix::Repository,
    branch_sha: &str,
    default_branch: Option<&str>,
) -> Integration {
    classify_with_cache(
        repo,
        branch_sha,
        default_branch,
        &IntegrationCache::default(),
    )
}

pub fn classify_with_cache(
    repo: &gix::Repository,
    branch_sha: &str,
    default_branch: Option<&str>,
    cache: &IntegrationCache,
) -> Integration {
    let Some(default_branch) = default_branch else {
        return Integration::unknown();
    };
    let Ok(branch_oid) = gix::ObjectId::from_hex(branch_sha.as_bytes()) else {
        return Integration::unknown();
    };
    match integration_reason_cached(repo, branch_oid, default_branch, cache) {
        Ok(Some(reason)) => Integration::integrated(Some(reason.as_str().into())),
        Ok(None) => Integration::not_integrated(),
        Err(_) => Integration::unknown(),
    }
}

pub fn integration_reason(
    repo: &gix::Repository,
    branch_oid: gix::ObjectId,
    target_name: &str,
) -> Result<Option<IntegrationReason>> {
    integration_reason_cached(repo, branch_oid, target_name, &IntegrationCache::default())
}

fn integration_reason_cached(
    repo: &gix::Repository,
    branch_oid: gix::ObjectId,
    target_name: &str,
    cache: &IntegrationCache,
) -> Result<Option<IntegrationReason>> {
    let target_ref = if target_name.starts_with("refs/") {
        target_name.to_string()
    } else {
        format!("refs/heads/{target_name}")
    };
    let Some(target_oid) = super::internals::peel_to_id(repo, &target_ref) else {
        return Ok(None);
    };

    let mut fallback_upstream = None;
    if let Some((_, _, up_oid)) = super::internals::branch_upstream(repo, target_name) {
        if up_oid != target_oid {
            // A stale local target must not override the newer upstream state.
            if is_ancestor(repo, target_oid, up_oid) == Some(true) {
                return check_against_cached(repo, branch_oid, up_oid, cache);
            }
            if is_ancestor(repo, up_oid, target_oid) != Some(true) {
                fallback_upstream = Some(up_oid);
            }
        }
    }
    if let Some(reason) = check_against_cached(repo, branch_oid, target_oid, cache)? {
        return Ok(Some(reason));
    }
    match fallback_upstream {
        Some(up_oid) => check_against_cached(repo, branch_oid, up_oid, cache),
        None => Ok(None),
    }
}

#[cfg(test)]
fn check_against(
    repo: &gix::Repository,
    branch: gix::ObjectId,
    target: gix::ObjectId,
) -> Result<Option<IntegrationReason>> {
    check_against_cached(repo, branch, target, &IntegrationCache::default())
}

fn check_against_cached(
    repo: &gix::Repository,
    branch: gix::ObjectId,
    target: gix::ObjectId,
    cache: &IntegrationCache,
) -> Result<Option<IntegrationReason>> {
    if branch == target {
        return Ok(Some(IntegrationReason::SameCommit));
    }
    // All remaining checks use the same merge base. Computing it once avoids
    // several complete graph traversals per worktree.
    let Some(base) = merge_base_oid(repo, target, branch) else {
        return Ok(None);
    };
    if base == branch {
        return Ok(Some(IntegrationReason::Ancestor));
    }
    if trees_match(repo, base, branch) == Some(true) {
        return Ok(Some(IntegrationReason::NoAddedChanges));
    }
    if trees_match(repo, branch, target) == Some(true) {
        return Ok(Some(IntegrationReason::TreesMatch));
    }
    match merge_probe_with_base(repo, base, branch, target) {
        MergeProbe::AddsNothing => return Ok(Some(IntegrationReason::MergeAddsNothing)),
        MergeProbe::Conflict => {
            if is_squash_via_patch_id(repo, base, branch, target, cache).unwrap_or(false) {
                return Ok(Some(IntegrationReason::PatchIdMatch));
            }
        }
        MergeProbe::WouldAdd | MergeProbe::Unknown => {}
    }
    Ok(None)
}

fn is_ancestor(repo: &gix::Repository, base: gix::ObjectId, head: gix::ObjectId) -> Option<bool> {
    if base == head {
        return Some(true);
    }
    let mb = repo.merge_base(base, head).ok()?;
    Some(mb.detach() == base)
}

fn merge_base_oid(
    repo: &gix::Repository,
    a: gix::ObjectId,
    b: gix::ObjectId,
) -> Option<gix::ObjectId> {
    repo.merge_base(a, b).ok().map(|id| id.detach())
}

fn trees_match(repo: &gix::Repository, a: gix::ObjectId, b: gix::ObjectId) -> Option<bool> {
    let ta = super::internals::tree_id_of(repo, &a)?;
    let tb = super::internals::tree_id_of(repo, &b)?;
    Some(ta == tb)
}

#[cfg(test)]
fn has_added_changes(
    repo: &gix::Repository,
    branch: gix::ObjectId,
    target: gix::ObjectId,
) -> Option<bool> {
    let base = merge_base_oid(repo, target, branch)?;
    // Tree identity includes modes, renames, and submodule entries too.
    trees_match(repo, base, branch).map(|same| !same)
}

enum MergeProbe {
    AddsNothing,
    WouldAdd,
    Conflict,
    Unknown,
}

#[cfg(test)]
fn merge_probe(repo: &gix::Repository, branch: gix::ObjectId, target: gix::ObjectId) -> MergeProbe {
    let Some(base) = merge_base_oid(repo, target, branch) else {
        return MergeProbe::WouldAdd;
    };
    merge_probe_with_base(repo, base, branch, target)
}

fn merge_probe_with_base(
    repo: &gix::Repository,
    base: gix::ObjectId,
    branch: gix::ObjectId,
    target: gix::ObjectId,
) -> MergeProbe {
    let Some(base_tree) = super::internals::tree_id_of(repo, &base) else {
        return MergeProbe::Unknown;
    };
    let Some(our_tree) = super::internals::tree_id_of(repo, &target) else {
        return MergeProbe::Unknown;
    };
    let Some(their_tree) = super::internals::tree_id_of(repo, &branch) else {
        return MergeProbe::Unknown;
    };

    let labels = gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some("base".into()),
        current: Some("ours".into()),
        other: Some("theirs".into()),
    };
    let opts = match repo.tree_merge_options() {
        Ok(o) => o,
        Err(_) => return MergeProbe::Unknown,
    };
    // Object-memory clone so simulated merge trees never hit disk.
    let merge_repo = repo.clone().with_object_memory();
    let outcome = match merge_repo.merge_trees(base_tree, our_tree, their_tree, labels, opts) {
        Ok(o) => o,
        Err(_) => return MergeProbe::Unknown,
    };
    let unresolved =
        outcome.has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default());
    if unresolved {
        return MergeProbe::Conflict;
    }
    let mut editor = outcome.tree;
    match editor.write() {
        Ok(id) => {
            if id.detach() == our_tree {
                MergeProbe::AddsNothing
            } else {
                MergeProbe::WouldAdd
            }
        }
        Err(_) => MergeProbe::Unknown,
    }
}

fn is_squash_via_patch_id(
    repo: &gix::Repository,
    base: gix::ObjectId,
    branch: gix::ObjectId,
    target: gix::ObjectId,
    cache: &IntegrationCache,
) -> Option<bool> {
    let commits = commits_in_range(repo, base, target)?;
    if commits.len() > PATCH_ID_SCAN_MAX_COMMITS {
        return Some(false);
    }
    let branch_pid = patch_id_between(repo, base, branch)?;
    if branch_pid.is_empty() {
        return Some(false);
    }
    for commit in commits {
        let patch_id = {
            let mut ids = cache.commit_patch_ids.lock().ok()?;
            ids.entry(commit)
                .or_insert_with(|| Arc::new(OnceLock::new()))
                .clone()
        };
        let pid = patch_id.get_or_init(|| {
            first_parent(repo, commit).and_then(|parent| patch_id_between(repo, parent, commit))
        });
        if pid.as_deref() == Some(branch_pid.as_str()) {
            return Some(true);
        }
    }
    Some(false)
}

fn commits_in_range(
    repo: &gix::Repository,
    base: gix::ObjectId,
    head: gix::ObjectId,
) -> Option<Vec<gix::ObjectId>> {
    // Read one extra commit to detect overflow before computing any patch IDs.
    repo.rev_walk([head])
        .with_hidden([base])
        .all()
        .ok()?
        .take(PATCH_ID_SCAN_MAX_COMMITS + 1)
        .map(|commit| commit.ok().map(|info| info.id))
        .collect()
}

fn first_parent(repo: &gix::Repository, commit: gix::ObjectId) -> Option<gix::ObjectId> {
    let obj = repo.find_object(commit).ok()?;
    let c = obj.try_into_commit().ok()?;
    let parent = c.parent_ids().next().map(|id| id.detach());
    parent
}

type BlobChange = (String, String, String);

fn tree_blob_changes(
    repo: &gix::Repository,
    old: gix::ObjectId,
    new: gix::ObjectId,
) -> Option<BTreeSet<BlobChange>> {
    let old_tree = repo
        .find_object(old)
        .ok()?
        .try_into_commit()
        .ok()
        .and_then(|c| c.tree_id().ok().map(|id| id.detach()))
        .or_else(|| {
            // already a tree?
            repo.find_object(old).ok().and_then(|o| {
                if o.kind == gix::object::Kind::Tree {
                    Some(old)
                } else {
                    None
                }
            })
        })?;
    let new_tree = repo
        .find_object(new)
        .ok()?
        .try_into_commit()
        .ok()
        .and_then(|c| c.tree_id().ok().map(|id| id.detach()))
        .or_else(|| {
            repo.find_object(new).ok().and_then(|o| {
                if o.kind == gix::object::Kind::Tree {
                    Some(new)
                } else {
                    None
                }
            })
        })?;

    let old_t = repo.find_object(old_tree).ok()?.try_into_tree().ok()?;
    let new_t = repo.find_object(new_tree).ok()?.try_into_tree().ok()?;
    let mut changes = BTreeSet::new();
    old_t
        .changes()
        .ok()?
        .options(|options| {
            options.track_path().track_rewrites(None);
        })
        .for_each_to_obtain_tree(&new_t, |change| {
            use gix::object::tree::diff::Change;
            match change {
                Change::Addition {
                    location,
                    id,
                    entry_mode,
                    ..
                } if entry_mode.is_blob() || entry_mode.is_link() => {
                    changes.insert((location.to_string(), String::new(), id.detach().to_string()));
                }
                Change::Deletion {
                    location,
                    id,
                    entry_mode,
                    ..
                } if entry_mode.is_blob() || entry_mode.is_link() => {
                    changes.insert((location.to_string(), id.detach().to_string(), String::new()));
                }
                Change::Modification {
                    location,
                    previous_id,
                    id,
                    previous_entry_mode,
                    entry_mode,
                    ..
                } if (entry_mode.is_blob() || entry_mode.is_link())
                    && (previous_entry_mode.is_blob() || previous_entry_mode.is_link()) =>
                {
                    changes.insert((
                        location.to_string(),
                        previous_id.detach().to_string(),
                        id.detach().to_string(),
                    ));
                }
                _ => {}
            }
            Ok::<_, std::convert::Infallible>(gix::object::tree::diff::Action::Continue(()))
        })
        .ok()?;
    Some(changes)
}

fn patch_id_between(
    repo: &gix::Repository,
    old: gix::ObjectId,
    new: gix::ObjectId,
) -> Option<String> {
    let changes = tree_blob_changes(repo, old, new)?;
    if changes.is_empty() {
        return Some(String::new());
    }
    let mut hasher = Sha1::new();
    for (path, old_id, new_id) in &changes {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(old_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(new_id.as_bytes());
        hasher.update(b"\n");
    }
    Some(hex::encode(hasher.finalize()))
}
