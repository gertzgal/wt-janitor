use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::activity;
use crate::config::{prune_min_age_days, Config, RepoConfig};
use crate::deps;
use crate::discovery::{self, RepoScan};
use crate::error::{Error, Result};
use crate::models::{
    DepDir, BLOCKED_DIRTY, INTEGRATED, INTEGRATION_UNKNOWN, REMOVE_MERGED, UNKNOWN,
};
use crate::proc;
use crate::state::{get_worktree_state, load_state, WorktreeState};

#[derive(Debug, Clone)]
pub struct MergePlanEntry {
    pub repo: String,
    pub path: String,
    pub branch: Option<String>,
    pub reason: Option<String>,
    pub blocked: bool,
    pub block_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MergePlan {
    pub repo_name: String,
    pub repo_path: Option<String>,
    pub candidates: Vec<MergePlanEntry>,
    pub blocked: Vec<MergePlanEntry>,
    pub wt_candidates: Vec<serde_json::Value>,
    pub apply_results: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub removed: i64,
    pub skipped: i64,
    pub failed: i64,
    pub safety_refused: bool,
    pub protected: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct DepPlanEntry {
    pub repo: String,
    pub worktree: String,
    pub branch: Option<String>,
    pub target: String,
    pub name: String,
    pub size_bytes: u64,
    pub is_symlink: bool,
    pub stale_at_plan: String,
    pub commit_ts: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DepPlan {
    pub repo_name: String,
    pub repo_path: PathBuf,
    pub entries: Vec<DepPlanEntry>,
    pub deletions: Vec<(String, String)>,
    pub errors: Vec<String>,
    pub deleted_bytes: u64,
    pub skipped_reactivated: i64,
    pub free_delta: Option<i64>,
}

pub fn plan_merged(
    config: &Config,
    repos: &[&RepoConfig],
    state: &serde_json::Value,
    do_fetch: bool,
) -> Vec<MergePlan> {
    let mut plans = Vec::new();
    let min_age = prune_min_age_days(config);
    for repo in repos {
        let mut plan = MergePlan {
            repo_name: repo.name.clone(),
            repo_path: None,
            candidates: Vec::new(),
            blocked: Vec::new(),
            wt_candidates: Vec::new(),
            apply_results: Vec::new(),
            errors: Vec::new(),
            removed: 0,
            skipped: 0,
            failed: 0,
            safety_refused: false,
            protected: HashMap::new(),
        };
        let scan = match discovery::discover_repo_for_merged(
            repo,
            state,
            config.inactive_days,
            do_fetch,
            discovery::now_secs(),
            min_age,
        ) {
            Ok(s) => s,
            Err(e) => {
                plan.errors.push(e.to_string());
                plans.push(plan);
                continue;
            }
        };
        plan.repo_path = Some(scan.repo_path.clone());
        fill_merged_plan(&mut plan, &scan, min_age);
        plans.push(plan);
    }
    plans
}

fn fill_merged_plan(plan: &mut MergePlan, scan: &RepoScan, min_age_days: i64) {
    for rec in &scan.worktrees {
        if rec.is_main {
            plan.protected
                .insert(rec.path.clone(), "main worktree (always kept)".into());
            continue;
        }
        if rec.missing {
            continue;
        }
        if rec.locked {
            plan.protected.insert(rec.path.clone(), "locked".into());
        }
        if rec.detached {
            plan.protected
                .insert(rec.path.clone(), "detached HEAD".into());
        }
        let reason = if rec.integration.state == INTEGRATED {
            rec.integration.reason.clone()
        } else {
            None
        };
        if rec.action == REMOVE_MERGED {
            plan.candidates.push(MergePlanEntry {
                repo: rec.repo.clone(),
                path: rec.path.clone(),
                branch: rec.branch.clone(),
                reason,
                blocked: false,
                block_reason: None,
            });
        } else if (rec.integration.state == INTEGRATED
            || rec.integration.state == INTEGRATION_UNKNOWN)
            && (rec.action == BLOCKED_DIRTY || rec.action == UNKNOWN)
        {
            let block = if let Some(op) = &rec.in_progress_op {
                format!("{op} in progress")
            } else if rec.dirty {
                format!("dirty worktree: {}", rec.dirty_kinds.join(", "))
            } else if !rec.action_detail.is_empty() {
                rec.action_detail.clone()
            } else {
                "uncertain integration state".into()
            };
            plan.blocked.push(MergePlanEntry {
                repo: rec.repo.clone(),
                path: rec.path.clone(),
                branch: rec.branch.clone(),
                reason,
                blocked: true,
                block_reason: Some(block),
            });
        }
    }

    let min_age_secs = min_age_days as f64 * 86400.0;
    for c in &plan.candidates {
        if plan.protected.contains_key(&c.path) {
            continue;
        }
        // Age filter matches Worktrunk's --min-age: skip young worktrees at apply time.
        let rec = scan.worktrees.iter().find(|r| r.path == c.path);
        if let Some(rec) = rec {
            if min_age_days > 0 && rec.inactivity.seconds < min_age_secs {
                continue;
            }
        }
        plan.wt_candidates.push(serde_json::json!({
            "branch": c.branch,
            "path": c.path,
            "reason": c.reason,
            "target": "worktree",
            "kind": "worktree",
        }));
    }
}

pub fn apply_merged(
    config: &Config,
    repos: &[&RepoConfig],
    state: &serde_json::Value,
    do_fetch: bool,
) -> Vec<MergePlan> {
    let mut plans = plan_merged(config, repos, state, do_fetch);
    for plan in &mut plans {
        let Some(repo_path) = plan.repo_path.clone() else {
            continue;
        };
        let repo_path = PathBuf::from(repo_path);
        let conflicts: Vec<_> = plan
            .wt_candidates
            .iter()
            .filter(|c| {
                c.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| {
                        let real = crate::sizes::realpath(Path::new(p));
                        plan.protected
                            .contains_key(&real.to_string_lossy().into_owned())
                            || plan.protected.contains_key(p)
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if !conflicts.is_empty() {
            plan.safety_refused = true;
            for c in &conflicts {
                let path = c.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let real = crate::sizes::realpath(Path::new(path));
                let reason = plan
                    .protected
                    .get(&real.to_string_lossy().into_owned())
                    .or_else(|| plan.protected.get(path))
                    .map(String::as_str)
                    .unwrap_or("protected");
                plan.errors.push(format!(
                    "safety refusal: prune would remove {path} but wt-janitor classifies it as {reason}"
                ));
            }
            continue;
        }

        for item in plan.wt_candidates.clone() {
            let path = item
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            let branch = item
                .get("branch")
                .and_then(|b| b.as_str())
                .map(str::to_string);
            let reason = item
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("integrated")
                .to_string();
            match remove_worktree(&repo_path, &path, branch.as_deref()) {
                Ok(()) => {
                    plan.removed += 1;
                    plan.apply_results.push(serde_json::json!({
                        "branch": branch,
                        "path": path,
                        "reason": reason,
                        "kind": "worktree",
                    }));
                }
                Err(e) => {
                    plan.failed += 1;
                    plan.apply_results.push(serde_json::json!({
                        "branch": branch,
                        "path": path,
                        "reason": reason,
                        "kind": "worktree",
                        "error": e.to_string(),
                    }));
                    plan.errors.push(e.to_string());
                }
            }
        }
    }
    plans
}

fn remove_worktree(repo: &Path, worktree: &str, branch: Option<&str>) -> Result<()> {
    // Never --force / -D / --yes. git worktree remove refuses dirty trees.
    proc::git(
        repo,
        &["worktree", "remove", "--", worktree],
        Duration::from_secs(120),
        true,
    )?;
    if let Some(branch) = branch {
        if let Ok(gix_repo) = crate::git::open_repo_operational(repo) {
            let full = format!("refs/heads/{branch}");
            if let Ok(r) = gix_repo.find_reference(&full) {
                let _ = r.delete();
            }
        }
    }
    Ok(())
}

pub fn plan_deps(
    config: &Config,
    repos: &[&RepoConfig],
    state: &serde_json::Value,
    inactive_days: Option<i64>,
) -> Vec<DepPlan> {
    let days = inactive_days.unwrap_or(config.inactive_days);
    let mut plans = Vec::new();
    for repo in repos {
        let mut plan = DepPlan {
            repo_name: repo.name.clone(),
            repo_path: repo.path.clone(),
            entries: Vec::new(),
            deletions: Vec::new(),
            errors: Vec::new(),
            deleted_bytes: 0,
            skipped_reactivated: 0,
            free_delta: None,
        };
        let scan = match discovery::discover_repo_for_dependencies(
            repo,
            state,
            days,
            &config.dependency_dirs,
            discovery::now_secs(),
        ) {
            Ok(s) => s,
            Err(e) => {
                plan.errors.push(e.to_string());
                plans.push(plan);
                continue;
            }
        };
        plan.repo_path = PathBuf::from(&scan.repo_path);
        for rec in &scan.worktrees {
            if rec.is_main || rec.missing || rec.detached || rec.locked {
                continue;
            }
            if !rec.inactive {
                continue;
            }
            for dep in &rec.sizes.dep_dirs {
                plan.entries.push(DepPlanEntry {
                    repo: rec.repo.clone(),
                    worktree: rec.path.clone(),
                    branch: rec.branch.clone(),
                    target: dep.path.clone(),
                    name: dep.name.clone(),
                    size_bytes: dep.size_bytes,
                    is_symlink: dep.is_symlink,
                    stale_at_plan: format!(
                        "{} ({})",
                        rec.inactivity.age(discovery::now_secs()),
                        rec.inactivity.source
                    ),
                    commit_ts: rec.commit_ts,
                });
            }
        }
        plans.push(plan);
    }
    plans
}

pub fn apply_deps(
    config: &Config,
    repos: &[&RepoConfig],
    state_path: &Path,
    inactive_days: Option<i64>,
) -> Vec<DepPlan> {
    let days = inactive_days.unwrap_or(config.inactive_days);
    let state = load_state(state_path);
    let mut plans = plan_deps(config, repos, &state, Some(days));
    let now = discovery::now_secs();
    let state = load_state(state_path);
    for plan in &mut plans {
        let free_before = free_space(&plan.repo_path);
        let mut seen: HashMap<String, bool> = HashMap::new();
        for entry in plan.entries.clone() {
            let active = *seen.entry(entry.worktree.clone()).or_insert_with(|| {
                let wt_state = get_worktree_state(&state, &entry.worktree);
                is_inactive(
                    Path::new(&entry.worktree),
                    entry.commit_ts,
                    wt_state.as_ref(),
                    days,
                    now,
                )
            });
            if !active {
                plan.deletions
                    .push((entry.target.clone(), "skipped — became active again".into()));
                plan.skipped_reactivated += 1;
                continue;
            }
            let candidate = DepDir {
                path: entry.target.clone(),
                name: entry.name.clone(),
                size_bytes: entry.size_bytes,
                is_symlink: entry.is_symlink,
            };
            match deps::delete_target(&candidate, Path::new(&entry.worktree), &plan.repo_path) {
                Ok(removed) => {
                    plan.deletions
                        .push((removed.to_string_lossy().into_owned(), "deleted".into()));
                    plan.deleted_bytes += entry.size_bytes;
                }
                Err(Error::Safety(msg)) => {
                    plan.deletions
                        .push((entry.target.clone(), format!("refused — {msg}")));
                    plan.errors
                        .push(format!("safety validation refused {}: {msg}", entry.target));
                }
                Err(e) => {
                    plan.deletions
                        .push((entry.target.clone(), format!("failed — {e}")));
                    plan.errors
                        .push(format!("failed to delete {}: {e}", entry.target));
                }
            }
        }
        let free_after = free_space(&plan.repo_path);
        if let (Some(b), Some(a)) = (free_before, free_after) {
            plan.free_delta = Some(a as i64 - b as i64);
        }
    }
    plans
}

fn is_inactive(
    worktree: &Path,
    commit_ts: Option<f64>,
    wt_state: Option<&WorktreeState>,
    days: i64,
    now: f64,
) -> bool {
    let inactivity =
        activity::compute_inactivity(worktree, commit_ts, wt_state, days, now, Some(3.0));
    inactivity.seconds >= days as f64 * 86400.0
}

fn free_space(path: &Path) -> Option<u64> {
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut s) == 0 {
            Some(s.f_bavail as u64 * s.f_frsize as u64)
        } else {
            None
        }
    }
}
