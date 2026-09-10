use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const ACTIVITY_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    "coverage",
    ".cache",
    ".pytest_cache",
    "__pycache__",
];

pub const DEFAULT_WALK_BUDGET_S: f64 = 5.0;

#[derive(Debug, Default, Clone)]
pub struct WalkResult {
    pub total_bytes: u64,
    pub max_mtime: Option<f64>,
    pub file_count: u64,
    pub incomplete: bool,
}

pub fn walk_tree(root: &Path, excludes: &[&str], budget_s: f64) -> WalkResult {
    let mut result = WalkResult::default();
    let deadline = Instant::now() + Duration::from_secs_f64(budget_s.max(0.0));
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        if Instant::now() > deadline {
            result.incomplete = true;
            break;
        }
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => {
                result.incomplete = true;
                continue;
            }
        };
        for entry in entries {
            if Instant::now() > deadline {
                result.incomplete = true;
                return result;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if ft.is_dir() {
                let n = name.to_string_lossy();
                if excludes.iter().any(|e| *e == n.as_ref()) {
                    continue;
                }
                stack.push(entry.path());
            } else {
                result.total_bytes += meta.len();
                result.file_count += 1;
                if let Ok(mtime) = meta.modified() {
                    if let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        let ts = d.as_secs_f64();
                        if ts > result.max_mtime.unwrap_or(0.0) {
                            result.max_mtime = Some(ts);
                        }
                    }
                }
            }
        }
    }
    result
}

#[derive(Debug, Clone)]
pub struct DepCandidate {
    pub path: PathBuf,
    pub name: String,
    pub is_symlink: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct DepScanResult {
    pub targets: Vec<DepCandidate>,
    pub incomplete: bool,
    pub total_bytes: u64,
    pub activity_max_mtime: Option<f64>,
}

pub fn find_dependency_dirs(
    worktree_root: &Path,
    names: &[String],
    budget_s: f64,
) -> DepScanResult {
    let wanted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut scan = DepScanResult::default();
    let deadline = Instant::now() + Duration::from_secs_f64(budget_s.max(0.0));
    // The boolean records whether this subtree is excluded from activity
    // timestamps. Size/dependency discovery still descends it so nested
    // allowlisted dependency directories remain visible.
    let mut stack = vec![(worktree_root.to_path_buf(), false)];
    while let Some((current, activity_excluded)) = stack.pop() {
        if Instant::now() > deadline {
            scan.incomplete = true;
            break;
        }
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => {
                scan.incomplete = true;
                continue;
            }
        };
        for entry in entries {
            if Instant::now() > deadline {
                scan.incomplete = true;
                return scan;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => {
                    scan.incomplete = true;
                    continue;
                }
            };
            let name = entry.file_name();
            let name_s = name.to_string_lossy().into_owned();
            if ft.is_symlink() {
                if wanted.contains(name_s.as_str()) {
                    scan.targets.push(DepCandidate {
                        path: entry.path(),
                        name: name_s,
                        is_symlink: true,
                        size_bytes: 0,
                    });
                }
                continue;
            }
            if name == ".git" {
                continue;
            }
            if ft.is_dir() {
                if wanted.contains(name_s.as_str()) {
                    let (size_bytes, complete) = measure_dir(&entry.path(), deadline);
                    scan.targets.push(DepCandidate {
                        path: entry.path(),
                        name: name_s,
                        is_symlink: false,
                        size_bytes,
                    });
                    if !complete {
                        scan.incomplete = true;
                        return scan;
                    }
                    continue;
                }
                let excluded = activity_excluded
                    || ACTIVITY_EXCLUDES.iter().any(|excluded| *excluded == name_s);
                stack.push((entry.path(), excluded));
            } else {
                let meta = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        scan.incomplete = true;
                        continue;
                    }
                };
                scan.total_bytes += meta.len();
                if !activity_excluded {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                            let timestamp = duration.as_secs_f64();
                            if timestamp > scan.activity_max_mtime.unwrap_or(0.0) {
                                scan.activity_max_mtime = Some(timestamp);
                            }
                        }
                    }
                }
            }
        }
    }
    scan
}

fn measure_dir(root: &Path, deadline: Instant) -> (u64, bool) {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        if Instant::now() > deadline {
            return (total, false);
        }
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => return (total, false),
        };
        for entry in entries {
            if Instant::now() > deadline {
                return (total, false);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return (total, false),
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => return (total, false),
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else {
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => return (total, false),
                };
                total += metadata.len();
            }
        }
    }
    (total, true)
}

pub fn human_bytes(n: u64) -> String {
    let mut value = n as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if value < 1024.0 || unit == "TB" {
            if unit == "B" {
                return format!("{} {unit}", value as u64);
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{n} B")
}

pub fn strictly_below(resolved: &Path, root: &Path) -> bool {
    resolved.starts_with(root) && resolved != root
}

pub fn realpath(path: &Path) -> PathBuf {
    match fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // Resolve existing prefix; keep the missing tail.
            resolve_missing(path)
        }
        Err(_) => path.to_path_buf(),
    }
}

fn resolve_missing(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut prefix = PathBuf::new();
    let mut rest = PathBuf::new();
    let mut seen_missing = false;
    for c in abs.components() {
        if seen_missing {
            rest.push(c);
            continue;
        }
        prefix.push(c);
        if !prefix.exists() {
            // last pushed doesn't exist
            prefix.pop();
            rest.push(c);
            seen_missing = true;
        }
    }
    let resolved = fs::canonicalize(&prefix).unwrap_or(prefix);
    if rest.as_os_str().is_empty() {
        resolved
    } else {
        resolved.join(rest)
    }
}
