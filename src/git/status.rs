use std::collections::BTreeSet;
use std::path::Path;

use gix::status::Item as StatusItem;

pub fn dirty_kinds(worktree: &Path) -> Vec<String> {
    let Ok(repo) = gix::open(worktree) else {
        return vec!["unknown".into()];
    };
    dirty_kinds_in(&repo)
}

pub fn dirty_kinds_in(repo: &gix::Repository) -> Vec<String> {
    let dirwalk = match repo.dirwalk_options() {
        Ok(options) => options,
        Err(_) => return vec!["unknown".into()],
    };
    let iter = match repo.status(gix::progress::Discard) {
        Ok(platform) => match platform
            // status.showUntrackedFiles=no removes the directory walk entirely.
            .index_worktree_options_mut(|options| options.dirwalk_options = Some(dirwalk))
            .untracked_files(gix::status::UntrackedFiles::Collapsed)
            .into_iter(Vec::<gix::bstr::BString>::new())
        {
            Ok(it) => it,
            Err(_) => return vec!["unknown".into()],
        },
        Err(_) => return vec!["unknown".into()],
    };

    let mut kinds = BTreeSet::new();
    for item in iter {
        let Ok(item) = item else {
            kinds.insert("unknown".into());
            continue;
        };
        match item {
            StatusItem::TreeIndex(_) => {
                kinds.insert("staged".into());
            }
            StatusItem::IndexWorktree(change) => match change {
                gix::status::index_worktree::Item::Modification { status, .. } => {
                    classify_entry_status(&status, &mut kinds);
                }
                gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                    if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                        kinds.insert("untracked".into());
                    }
                }
                gix::status::index_worktree::Item::Rewrite { copy, .. } => {
                    if copy {
                        kinds.insert("untracked".into());
                    } else {
                        kinds.insert("renamed".into());
                    }
                }
            },
        }
    }
    kinds.into_iter().collect()
}

fn classify_entry_status(
    status: &gix::status::plumbing::index_as_worktree::EntryStatus<(), gix::submodule::Status>,
    kinds: &mut BTreeSet<String>,
) {
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
    match status {
        EntryStatus::Conflict { .. } => {
            kinds.insert("modified".into());
        }
        EntryStatus::Change(change) => match change {
            Change::Removed => {
                kinds.insert("deleted".into());
            }
            Change::Type { .. }
            | Change::Modification { .. }
            | Change::SubmoduleModification(_) => {
                kinds.insert("modified".into());
            }
        },
        EntryStatus::NeedsUpdate(_) => {}
        EntryStatus::IntentToAdd => {
            kinds.insert("staged".into());
        }
    }
}
