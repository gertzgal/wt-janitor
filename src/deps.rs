use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::models::DepDir;
use crate::sizes::{realpath, strictly_below};

pub fn validate_target(
    candidate: &DepDir,
    worktree_root: &Path,
    repo_root: &Path,
) -> Result<PathBuf> {
    let raw = Path::new(&candidate.path);
    let raw_s = candidate.path.as_str();
    if raw_s.is_empty() || raw_s == "." || raw_s == ".." {
        return Err(Error::safety(format!(
            "empty or relative path refused: '{raw_s}'"
        )));
    }
    let root = realpath(worktree_root);

    if candidate.is_symlink {
        let parent = raw
            .parent()
            .map(|p| {
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    std::env::current_dir().unwrap_or_default().join(p)
                };
                realpath(&abs)
            })
            .unwrap_or_else(|| PathBuf::from("/"));
        if parent != root && !strictly_below(&parent, &root) {
            return Err(Error::safety(format!(
                "symlink sits outside the worktree root: '{raw_s}'"
            )));
        }
    } else {
        let resolved = realpath(raw);
        require_strictly_below(&resolved, &root, raw_s)?;
        require_not_protected(&resolved, raw_s, repo_root, worktree_root)?;
    }

    let parent = raw.parent().unwrap_or(Path::new("."));
    let parent_abs = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(parent)
    };
    let parent_real = realpath(&parent_abs);
    if parent_real != root && !strictly_below(&parent_real, &root) {
        return Err(Error::safety(format!(
            "target escapes the worktree root: '{raw_s}'"
        )));
    }

    if candidate.is_symlink {
        Ok(if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(raw)
        })
    } else {
        Ok(realpath(raw))
    }
}

fn require_strictly_below(resolved: &Path, root: &Path, raw: &str) -> Result<()> {
    if resolved == root {
        return Err(Error::safety(format!(
            "refusing to delete the worktree root itself: '{raw}'"
        )));
    }
    if !strictly_below(resolved, root) {
        return Err(Error::safety(format!(
            "target resolves outside the registered worktree root: '{raw}' -> {}",
            resolved.display()
        )));
    }
    Ok(())
}

fn require_not_protected(
    resolved: &Path,
    raw: &str,
    repo_root: &Path,
    worktree_root: &Path,
) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let protected = [
        realpath(Path::new("/")),
        realpath(&home),
        realpath(repo_root),
        realpath(worktree_root),
    ];
    if protected.iter().any(|p| p == resolved) {
        return Err(Error::safety(format!(
            "refusing to delete a protected root: '{raw}' -> {}",
            resolved.display()
        )));
    }
    Ok(())
}

pub fn delete_target(
    candidate: &DepDir,
    worktree_root: &Path,
    repo_root: &Path,
) -> Result<PathBuf> {
    let validated = validate_target(candidate, worktree_root, repo_root)?;
    if candidate.is_symlink || validated.is_symlink() {
        fs::remove_file(&validated)?;
    } else {
        if !validated.is_dir() {
            return Err(Error::safety(format!(
                "target disappeared or is not a directory: {}",
                validated.display()
            )));
        }
        fs::remove_dir_all(&validated)?;
    }
    Ok(validated)
}
