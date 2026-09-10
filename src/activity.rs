use std::path::Path;

use crate::models::Inactivity;
use crate::sizes::{self, ACTIVITY_EXCLUDES, DEFAULT_WALK_BUDGET_S};
use crate::state::WorktreeState;

pub fn compute_inactivity(
    worktree_root: &Path,
    commit_ts: Option<f64>,
    wt_state: Option<&WorktreeState>,
    inactive_days: i64,
    now: f64,
    budget_s: Option<f64>,
) -> Inactivity {
    let threshold = inactive_days as f64 * 86400.0;
    let walk = if commit_ts.is_none() || (now - commit_ts.unwrap()) > threshold {
        Some(sizes::walk_tree(
            worktree_root,
            ACTIVITY_EXCLUDES,
            budget_s.unwrap_or(DEFAULT_WALK_BUDGET_S),
        ))
    } else {
        None
    };
    compute_inactivity_from_walk(commit_ts, wt_state, inactive_days, now, walk.as_ref())
}

pub fn compute_inactivity_from_walk(
    commit_ts: Option<f64>,
    wt_state: Option<&WorktreeState>,
    inactive_days: i64,
    now: f64,
    walk: Option<&sizes::WalkResult>,
) -> Inactivity {
    let mut candidates: Vec<(f64, &str, bool)> = Vec::new();

    if let Some(ts) = commit_ts {
        candidates.push((ts, "last commit", false));
    }

    let threshold = inactive_days as f64 * 86400.0;
    if let Some(walk) = walk {
        if let Some(mtime) = walk.max_mtime {
            candidates.push((mtime, "source file mtime", walk.incomplete));
        } else if walk.incomplete {
            candidates.push((
                now - threshold - 1.0,
                "source file mtime (scan incomplete)",
                true,
            ));
        }
    }

    if let Some(st) = wt_state {
        candidates.push((st.first_seen, "first seen", false));
        if let Some(last) = st.last_used {
            candidates.push((last, "explicit touch", false));
        }
    }

    if candidates.is_empty() {
        return Inactivity {
            seconds: 0.0,
            source: "unknown".into(),
            incomplete: true,
            just_discovered: false,
        };
    }

    let (newest_ts, source, incomplete) = candidates
        .into_iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    let seconds = (now - newest_ts).max(0.0);
    let just_discovered = source == "first seen" && seconds < threshold;
    Inactivity {
        seconds,
        source: source.into(),
        incomplete,
        just_discovered,
    }
}
