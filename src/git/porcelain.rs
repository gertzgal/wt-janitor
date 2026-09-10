use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;
use crate::proc;
use crate::sizes::realpath;

#[derive(Debug, Clone)]
pub struct PorcelainWorktree {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub locked_reason: Option<String>,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
}

const PORCELAIN_FIELDS: &[&str] = &[
    "worktree", "HEAD", "branch", "bare", "detached", "locked", "prunable",
];

pub fn worktree_list_porcelain(repo: &Path) -> Result<Vec<PorcelainWorktree>> {
    let result = proc::git(
        repo,
        &["worktree", "list", "--porcelain", "-z"],
        Duration::from_secs(120),
        true,
    )?;
    parse_porcelain(&result.stdout)
}

pub fn parse_porcelain(stdout: &str) -> Result<Vec<PorcelainWorktree>> {
    let mut entries = Vec::new();
    let mut fields: Vec<(String, Option<String>)> = Vec::new();

    let flush = |fields: &mut Vec<(String, Option<String>)>,
                 entries: &mut Vec<PorcelainWorktree>| {
        let path = match fields.iter().find(|(k, _)| k == "worktree") {
            Some((_, Some(p))) => p.clone(),
            _ => {
                fields.clear();
                return;
            }
        };
        let get = |key: &str| -> Option<String> {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.clone())
        };
        let has = |key: &str| fields.iter().any(|(k, _)| k == key);
        let branch = get("branch").and_then(|b| {
            let stripped = b.strip_prefix("refs/heads/").unwrap_or(&b);
            if stripped.is_empty() {
                None
            } else {
                Some(stripped.to_string())
            }
        });
        entries.push(PorcelainWorktree {
            path: realpath(Path::new(&path)),
            head: get("HEAD"),
            branch,
            bare: has("bare"),
            detached: has("detached"),
            locked: has("locked"),
            locked_reason: get("locked"),
            prunable: has("prunable"),
            prunable_reason: get("prunable"),
        });
        fields.clear();
    };

    for token in stdout.split('\0') {
        if token.is_empty() {
            continue;
        }
        let (key, value) = match token.split_once(' ') {
            Some((k, v)) => (
                k,
                if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                },
            ),
            None => (token, None),
        };
        if key == "worktree" {
            flush(&mut fields, &mut entries);
        }
        if PORCELAIN_FIELDS.contains(&key) {
            fields.push((key.to_string(), value));
        }
    }
    flush(&mut fields, &mut entries);
    Ok(entries)
}
