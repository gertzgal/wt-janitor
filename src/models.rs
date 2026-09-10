use serde::Serialize;

pub const REMOVE_MERGED: &str = "REMOVE_MERGED";
pub const PURGE_DEPS: &str = "PURGE_DEPS";
pub const REVIEW_STALE: &str = "REVIEW_STALE";
pub const KEEP_ACTIVE: &str = "KEEP_ACTIVE";
pub const BLOCKED_DIRTY: &str = "BLOCKED_DIRTY";
pub const BLOCKED_LOCKED: &str = "BLOCKED_LOCKED";
pub const BLOCKED_DETACHED: &str = "BLOCKED_DETACHED";
pub const MISSING: &str = "MISSING";
pub const UNKNOWN: &str = "UNKNOWN";

pub const INTEGRATED: &str = "integrated";
pub const NOT_INTEGRATED: &str = "not-integrated";
pub const INTEGRATION_UNKNOWN: &str = "unknown";

pub fn days_ago(ts: f64, now: f64) -> String {
    let seconds = (now - ts).max(0.0) as i64;
    if seconds < 60 {
        return "just now".into();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", seconds / 86400)
}

#[derive(Debug, Clone, Default)]
pub struct Integration {
    pub state: String,
    pub reason: Option<String>,
}

impl Integration {
    pub fn unknown() -> Self {
        Self {
            state: INTEGRATION_UNKNOWN.into(),
            reason: None,
        }
    }

    pub fn integrated(reason: Option<String>) -> Self {
        Self {
            state: INTEGRATED.into(),
            reason,
        }
    }

    pub fn not_integrated() -> Self {
        Self {
            state: NOT_INTEGRATED.into(),
            reason: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Inactivity {
    pub seconds: f64,
    pub source: String,
    pub incomplete: bool,
    pub just_discovered: bool,
}

impl Inactivity {
    pub fn age(&self, now: f64) -> String {
        if self.just_discovered {
            return "just discovered".into();
        }
        days_ago(now - self.seconds, now)
    }
}

#[derive(Debug, Clone)]
pub struct DepDir {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub is_symlink: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SizeInfo {
    pub total_bytes: u64,
    pub complete: bool,
    pub reclaimable_bytes: u64,
    pub dep_dirs: Vec<DepDir>,
}

#[derive(Debug, Clone)]
pub struct WorktreeRecord {
    pub repo: String,
    pub repo_path: String,
    pub path: String,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub short_sha: Option<String>,
    pub commit_message: Option<String>,
    pub commit_ts: Option<f64>,
    pub is_main: bool,
    pub is_current: bool,
    pub locked: bool,
    pub detached: bool,
    pub missing: bool,
    pub dirty: bool,
    pub dirty_kinds: Vec<String>,
    pub default_branch: Option<String>,
    pub main_ahead: Option<i64>,
    pub main_behind: Option<i64>,
    pub remote_name: Option<String>,
    pub remote_ahead: Option<i64>,
    pub remote_behind: Option<i64>,
    pub integration: Integration,
    pub in_progress_op: Option<String>,
    pub worktree_state_note: Option<String>,
    pub inactivity: Inactivity,
    pub inactive: bool,
    pub sizes: SizeInfo,
    pub action: String,
    pub action_detail: String,
    pub notes: Vec<String>,
}

pub fn sha_short(sha: Option<&str>) -> Option<String> {
    sha.map(|s| s.chars().take(9).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationJson {
    pub state: String,
    pub reason: Option<String>,
}
