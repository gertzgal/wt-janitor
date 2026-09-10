use std::fs;
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

use crate::error::{Error, Result};
use crate::sizes::realpath;

pub fn open_repo(path: &Path) -> Result<gix::Repository> {
    gix::open(path).map_err(|e| {
        Error::config(format!(
            "path is not a Git repository: {}: {e}",
            path.display()
        ))
    })
}

pub fn open_repo_operational(path: &Path) -> Result<gix::Repository> {
    gix::open(path)
        .map_err(|e| Error::operational(format!("failed to open {}: {e}", path.display())))
}

pub fn is_git_repo(path: &Path) -> bool {
    gix::open(path).is_ok()
}

pub fn repo_identity(repo: &gix::Repository) -> PathBuf {
    realpath(repo.common_dir())
}

pub fn is_main_worktree(repo: &gix::Repository) -> bool {
    realpath(repo.git_dir()) == realpath(repo.common_dir())
}

pub fn main_worktree_path(repo: &gix::Repository) -> PathBuf {
    let identity = realpath(repo.common_dir());
    if identity.file_name().and_then(|n| n.to_str()) == Some(".git") {
        identity.parent().map(Path::to_path_buf).unwrap_or(identity)
    } else {
        identity
    }
}

pub fn default_branch(repo: &gix::Repository) -> Option<String> {
    if let Ok(r) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(name) = r.target().try_name() {
            let s = name.as_bstr().to_str_lossy();
            if let Some(rest) = s.strip_prefix("refs/remotes/origin/") {
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
            if let Some(rest) = s.strip_prefix("origin/") {
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    for candidate in ["main", "master"] {
        if repo
            .find_reference(&format!("refs/heads/{candidate}"))
            .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

pub fn worktree_git_dir(worktree: &Path) -> Option<PathBuf> {
    let git = worktree.join(".git");
    if git.is_dir() {
        return Some(git);
    }
    if git.is_file() {
        let text = fs::read_to_string(&git).ok()?;
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("gitdir:") {
                let p = PathBuf::from(rest.trim());
                if p.is_absolute() {
                    return Some(p);
                }
                return Some(worktree.join(p));
            }
        }
    }
    None
}

const IN_PROGRESS: &[(&str, &str)] = &[
    ("MERGE_HEAD", "merge"),
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase (am)"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
    ("BISECT_LOG", "bisect"),
];

pub fn in_progress_operation(worktree: &Path) -> Option<String> {
    let git_dir = worktree_git_dir(worktree)?;
    for (marker, name) in IN_PROGRESS {
        if git_dir.join(marker).exists() {
            return Some((*name).to_string());
        }
    }
    None
}

pub fn commit_info(
    repo: &gix::Repository,
    sha: &str,
) -> (Option<String>, Option<String>, Option<f64>) {
    let Ok(oid) = gix::ObjectId::from_hex(sha.as_bytes()) else {
        return (None, None, None);
    };
    let Ok(obj) = repo.find_object(oid) else {
        return (None, None, None);
    };
    let Ok(commit) = obj.try_into_commit() else {
        return (None, None, None);
    };
    let msg = commit.message_raw().ok().and_then(|m| {
        m.to_str()
            .ok()
            .map(|s| s.lines().next().unwrap_or("").to_string())
    });
    let ts = commit.time().ok().map(|time| time.seconds as f64);
    (Some(sha.to_string()), msg, ts)
}

pub fn tree_id_of(repo: &gix::Repository, sha: &gix::ObjectId) -> Option<gix::ObjectId> {
    let obj = repo.find_object(*sha).ok()?;
    let commit = obj.try_into_commit().ok()?;
    commit.tree_id().ok().map(|id| id.detach())
}

pub fn peel_to_id(repo: &gix::Repository, name: &str) -> Option<gix::ObjectId> {
    let r = repo.find_reference(name).ok()?;
    r.into_fully_peeled_id().ok().map(|id| id.detach())
}

pub fn branch_upstream(
    repo: &gix::Repository,
    branch: &str,
) -> Option<(String, String, gix::ObjectId)> {
    let full = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    let r = repo.find_reference(&full).ok()?;
    let up = r
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)?
        .ok()?;
    let up_s = up.to_string();
    let oid = peel_to_id(repo, &up_s)?;
    let remote_name = r
        .remote_name(gix::remote::Direction::Fetch)?
        .as_bstr()
        .to_string();
    Some((remote_name, up_s, oid))
}

pub fn ahead_behind(
    repo: &gix::Repository,
    local: gix::ObjectId,
    other: gix::ObjectId,
) -> Option<(i64, i64)> {
    if local == other {
        return Some((0, 0));
    }
    let ahead = count_not_in(repo, local, other)?;
    let behind = count_not_in(repo, other, local)?;
    Some((ahead as i64, behind as i64))
}

fn count_not_in(
    repo: &gix::Repository,
    from: gix::ObjectId,
    exclude: gix::ObjectId,
) -> Option<usize> {
    // Let the revision walker prune the excluded history instead of first
    // materializing every ancestor of `exclude`. On large, long-lived
    // repositories that turns the common "one commit ahead" case from a
    // full-history walk into a one-commit walk.
    repo.rev_walk([from])
        .with_hidden([exclude])
        .all()
        .ok()?
        .try_fold(0usize, |count, commit| commit.ok().map(|_| count + 1))
}

pub fn fetch(repo: &Path) -> crate::proc::CmdResult {
    match crate::proc::git(
        repo,
        &["fetch", "--all", "--prune"],
        std::time::Duration::from_secs(600),
        false,
    ) {
        Ok(r) => r,
        Err(e) => crate::proc::CmdResult {
            args: vec![
                "git".into(),
                "-C".into(),
                repo.display().to_string(),
                "fetch".into(),
            ],
            returncode: 1,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}
