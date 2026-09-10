use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::activity;
use crate::config::RepoConfig;
use crate::error::{Error, Result};
use crate::git;
use crate::models::{
    sha_short, DepDir, Inactivity, Integration, SizeInfo, WorktreeRecord, BLOCKED_DETACHED,
    BLOCKED_DIRTY, BLOCKED_LOCKED, INTEGRATED, INTEGRATION_UNKNOWN, KEEP_ACTIVE, MISSING,
    PURGE_DEPS, REMOVE_MERGED, REVIEW_STALE, UNKNOWN,
};
use crate::sizes::{self, realpath};
use crate::state::{get_worktree_state, WorktreeState};

pub const DEP_BUDGET_S: f64 = 5.0;

#[derive(Clone, Copy)]
enum DiscoveryMode {
    Full,
    Merged { min_age_days: i64 },
    Dependencies,
}

impl DiscoveryMode {
    fn needs_integration(self) -> bool {
        !matches!(self, Self::Dependencies)
    }

    fn needs_inactivity(self) -> bool {
        !matches!(self, Self::Merged { min_age_days: 0 })
    }

    fn needs_ahead_behind(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Debug, Clone)]
pub struct RepoScan {
    pub repo_name: String,
    pub repo_path: String,
    pub default_branch: Option<String>,
    pub identity: String,
    pub is_main: bool,
    pub fetch_ok: Option<bool>,
    pub fetch_stale: bool,
    pub worktrees: Vec<WorktreeRecord>,
    pub errors: Vec<String>,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn resolve_repo_path(repo: &RepoConfig) -> Result<PathBuf> {
    let path = realpath(&repo.path);
    if !path.is_dir() {
        return Err(Error::operational(format!(
            "repository path does not exist: {}",
            path.display()
        )));
    }
    if !git::is_git_repo(&path) {
        return Err(Error::config(format!(
            "path is not a Git repository: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub fn discover_repo(
    repo: &RepoConfig,
    state: &serde_json::Value,
    inactive_days: i64,
    dependency_dirs: &[String],
    do_fetch: bool,
    now: f64,
) -> Result<RepoScan> {
    discover_repo_with_mode(
        repo,
        state,
        inactive_days,
        dependency_dirs,
        do_fetch,
        now,
        DiscoveryMode::Full,
    )
}

pub(crate) fn discover_repo_for_merged(
    repo: &RepoConfig,
    state: &serde_json::Value,
    inactive_days: i64,
    do_fetch: bool,
    now: f64,
    min_age_days: i64,
) -> Result<RepoScan> {
    discover_repo_with_mode(
        repo,
        state,
        inactive_days,
        &[],
        do_fetch,
        now,
        DiscoveryMode::Merged { min_age_days },
    )
}

pub(crate) fn discover_repo_for_dependencies(
    repo: &RepoConfig,
    state: &serde_json::Value,
    inactive_days: i64,
    dependency_dirs: &[String],
    now: f64,
) -> Result<RepoScan> {
    discover_repo_with_mode(
        repo,
        state,
        inactive_days,
        dependency_dirs,
        false,
        now,
        DiscoveryMode::Dependencies,
    )
}

#[allow(clippy::too_many_arguments)]
fn discover_repo_with_mode(
    repo: &RepoConfig,
    state: &serde_json::Value,
    inactive_days: i64,
    dependency_dirs: &[String],
    do_fetch: bool,
    now: f64,
    mode: DiscoveryMode,
) -> Result<RepoScan> {
    let repo_path = resolve_repo_path(repo)?;
    let gix_repo = git::open_repo_operational(&repo_path)?;
    let identity = git::repo_identity(&gix_repo);
    let is_main = git::is_main_worktree(&gix_repo);
    let default_branch = git::default_branch(&gix_repo);
    let mut scan = RepoScan {
        repo_name: repo.name.clone(),
        repo_path: repo_path.to_string_lossy().into_owned(),
        default_branch: default_branch.clone(),
        identity: identity.to_string_lossy().into_owned(),
        is_main,
        fetch_ok: None,
        fetch_stale: false,
        worktrees: Vec::new(),
        errors: Vec::new(),
    };

    if !is_main {
        scan.errors.push(format!(
            "configured path is not the main worktree (git dir mismatch): {}",
            repo_path.display()
        ));
    }

    if do_fetch {
        let fetch_result = git::fetch(&repo_path);
        scan.fetch_ok = Some(fetch_result.ok());
        scan.fetch_stale = !fetch_result.ok();
        if !fetch_result.ok() {
            let msg = fetch_result.stderr.trim();
            scan.errors.push(format!(
                "fetch failed — integration information may be stale: {}",
                if msg.is_empty() {
                    "no output"
                } else {
                    &msg[..msg.len().min(200)]
                }
            ));
        }
    }

    let porcelain = match git::worktree_list_porcelain(&repo_path) {
        Ok(p) => p,
        Err(e) => {
            scan.errors.push(format!("git worktree list failed: {e}"));
            Vec::new()
        }
    };

    let main_path = git::main_worktree_path(&gix_repo);
    let cwd = std::env::current_dir().ok().map(|p| realpath(&p));

    let integration_cache = git::IntegrationCache::default();
    let worktrees = porcelain
        .iter()
        .filter(|worktree| !worktree.bare)
        .collect::<Vec<_>>();
    let workers = if worktrees.is_empty() {
        1
    } else {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .saturating_mul(8)
            .min(worktrees.len())
            .min(32)
    };
    let chunk_size = worktrees.len().div_ceil(workers).max(1);
    let mut built = Vec::with_capacity(worktrees.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for (chunk_index, chunk) in worktrees.chunks(chunk_size).enumerate() {
            let repo_path = &repo_path;
            let main_path = &main_path;
            let cwd = cwd.as_deref();
            let scan = &scan;
            let integration_cache = &integration_cache;
            handles.push(scope.spawn(move || {
                let thread_repo = git::open_repo_operational(repo_path)
                    .expect("repository remained available during discovery");
                chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, pw)| {
                        let started = Instant::now();
                        let record = build_record(
                            repo,
                            repo_path,
                            &thread_repo,
                            pw,
                            main_path,
                            cwd,
                            get_worktree_state(state, &pw.path.to_string_lossy()),
                            scan,
                            now,
                            mode,
                            integration_cache,
                        );
                        (chunk_index * chunk_size + offset, record, started.elapsed())
                    })
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            if let Ok(mut records) = handle.join() {
                built.append(&mut records);
            }
        }
    });
    built.sort_unstable_by_key(|(index, _, _)| *index);
    for (_, record, elapsed) in built {
        if crate::proc::verbose() && elapsed.as_millis() >= 10 {
            eprintln!(
                "discovery metadata/integration: {} ms ({})",
                elapsed.as_millis(),
                record.path
            );
        }
        scan.worktrees.push(record);
    }

    let enrichment_started = Instant::now();
    match mode {
        DiscoveryMode::Full | DiscoveryMode::Dependencies => enrich_filesystem_records(
            &mut scan.worktrees,
            state,
            inactive_days,
            dependency_dirs,
            now,
            matches!(mode, DiscoveryMode::Full),
        ),
        DiscoveryMode::Merged { .. } => enrich_merged_statuses(&mut scan.worktrees),
    }
    if crate::proc::verbose() {
        eprintln!(
            "filesystem enrichment: {} ms",
            enrichment_started.elapsed().as_millis()
        );
    }

    if let Some(cwd) = cwd {
        if let Some(index) = scan
            .worktrees
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                let path = Path::new(&record.path);
                cwd == path || cwd.starts_with(path)
            })
            .max_by_key(|(_, record)| Path::new(&record.path).components().count())
            .map(|(index, _)| index)
        {
            for (record_index, record) in scan.worktrees.iter_mut().enumerate() {
                record.is_current = record_index == index;
            }
        }
    }

    Ok(scan)
}

#[allow(clippy::too_many_arguments)]
fn build_record(
    repo: &RepoConfig,
    repo_path: &Path,
    gix_repo: &gix::Repository,
    pw: &git::PorcelainWorktree,
    main_path: &Path,
    cwd: Option<&Path>,
    wt_state: Option<WorktreeState>,
    scan: &RepoScan,
    now: f64,
    mode: DiscoveryMode,
    integration_cache: &git::IntegrationCache,
) -> WorktreeRecord {
    let path = pw.path.clone();
    let path_s = path.to_string_lossy().into_owned();
    let missing = !path.is_dir();
    let is_main = path == main_path;
    let is_current = cwd
        .map(|c| c == path.as_path() || c.starts_with(&path))
        .unwrap_or(false);

    let (head_sha, commit_message, commit_ts) = match &pw.head {
        Some(sha) if !missing && mode.needs_inactivity() => {
            let (_full, msg, ts) = git::commit_info(gix_repo, sha);
            (Some(sha.clone()), msg, ts)
        }
        Some(sha) => (Some(sha.clone()), None, None),
        None => (None, None, None),
    };

    let skip_integration = !mode.needs_integration()
        || (matches!(mode, DiscoveryMode::Merged { .. })
            && (is_main || missing || pw.locked || pw.detached));
    let integration = if skip_integration {
        Integration::unknown()
    } else if let Some(sha) = pw.head.as_deref() {
        git::classify_integration_with_cache(
            gix_repo,
            sha,
            scan.default_branch.as_deref(),
            integration_cache,
        )
    } else {
        Integration::unknown()
    };

    let in_progress_op = if missing {
        None
    } else {
        git::in_progress_operation(&path)
    };
    // Filesystem work is performed in bounded parallel batches after the
    // cheap repository-level checks have completed.
    let dirty_kinds = Vec::new();
    let dirty = false;
    let (main_ahead, main_behind) = if mode.needs_ahead_behind() {
        match (pw.head.as_deref(), scan.default_branch.as_deref()) {
            (Some(sha), Some(branch)) => {
                if let (Ok(local), Some(target)) = (
                    gix::ObjectId::from_hex(sha.as_bytes()),
                    git::peel_to_id(gix_repo, &format!("refs/heads/{branch}")),
                ) {
                    git::ahead_behind(gix_repo, local, target)
                } else {
                    None
                }
            }
            _ => None,
        }
        .map(|(a, b)| (Some(a), Some(b)))
        .unwrap_or((None, None))
    } else {
        (None, None)
    };

    let (remote_name, remote_ahead, remote_behind) = if mode.needs_ahead_behind() {
        if let Some(branch) = pw.branch.as_deref() {
            if let Some((name, _up, oid)) = git::branch_upstream(gix_repo, branch) {
                if let Some(sha) = pw.head.as_deref() {
                    if let Ok(local) = gix::ObjectId::from_hex(sha.as_bytes()) {
                        let ab = git::ahead_behind(gix_repo, local, oid);
                        match ab {
                            Some((a, b)) => (Some(name), Some(a), Some(b)),
                            None => (Some(name), None, None),
                        }
                    } else {
                        (Some(name), None, None)
                    }
                } else {
                    (Some(name), None, None)
                }
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        }
    } else {
        (None, None, None)
    };

    let mut record = WorktreeRecord {
        repo: repo.name.clone(),
        repo_path: realpath(repo_path).to_string_lossy().into_owned(),
        path: path_s.clone(),
        branch: pw.branch.clone(),
        head_sha: head_sha.clone(),
        short_sha: sha_short(head_sha.as_deref()),
        commit_message,
        commit_ts,
        is_main: path == main_path,
        is_current,
        locked: pw.locked,
        detached: pw.detached,
        missing,
        dirty,
        dirty_kinds,
        default_branch: scan.default_branch.clone(),
        main_ahead,
        main_behind,
        remote_name,
        remote_ahead,
        remote_behind,
        integration,
        in_progress_op,
        worktree_state_note: if pw.prunable {
            pw.prunable_reason.clone()
        } else {
            None
        },
        inactivity: Inactivity {
            seconds: 0.0,
            source: "missing".into(),
            incomplete: true,
            just_discovered: false,
        },
        inactive: false,
        sizes: SizeInfo {
            complete: true,
            ..SizeInfo::default()
        },
        action: UNKNOWN.into(),
        action_detail: String::new(),
        notes: Vec::new(),
    };

    if let DiscoveryMode::Merged { min_age_days } = mode {
        if !missing && min_age_days > 0 {
            record.inactivity =
                compute_merged_inactivity(&path, commit_ts, wt_state.as_ref(), min_age_days, now);
            record.inactive = record.inactivity.seconds >= min_age_days as f64 * 86400.0;
        }
    }

    record.action = recommend(&record);
    record
}

fn compute_merged_inactivity(
    path: &Path,
    commit_ts: Option<f64>,
    wt_state: Option<&WorktreeState>,
    inactive_days: i64,
    now: f64,
) -> Inactivity {
    // A clean integrated worktree does not need an exhaustive source walk to
    // establish minimum age. The root and index mtimes capture checkout and
    // Git activity, while persisted first-seen/touch times provide the same
    // newly-discovered shield used by a full scan.
    let root_mtime = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64());
    let index_mtime = git::internals::worktree_git_dir(path)
        .and_then(|git_dir| std::fs::metadata(git_dir.join("index")).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64());
    let walk = sizes::WalkResult {
        max_mtime: root_mtime
            .into_iter()
            .chain(index_mtime)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)),
        incomplete: root_mtime.is_none() && index_mtime.is_none(),
        ..sizes::WalkResult::default()
    };
    activity::compute_inactivity_from_walk(commit_ts, wt_state, inactive_days, now, Some(&walk))
}

fn enrich_merged_statuses(records: &mut [WorktreeRecord]) {
    let jobs = records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            !record.missing
                && !record.is_main
                && !record.locked
                && !record.detached
                && record.in_progress_op.is_none()
                && record.integration.state == INTEGRATED
        })
        .map(|(index, record)| (index, PathBuf::from(&record.path)))
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return;
    }

    // A failed worker must conservatively block removal rather than leaving
    // the optimistic placeholder status in place.
    for (index, _) in &jobs {
        records[*index].dirty = true;
        records[*index].dirty_kinds = vec!["unknown".into()];
        records[*index].action = recommend(&records[*index]);
    }

    // Status spends much of its time waiting on filesystem metadata. A modest
    // oversubscription keeps those waits from serializing the whole plan.
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .saturating_mul(8)
        .min(jobs.len())
        .min(32);
    let chunk_size = jobs.len().div_ceil(workers);
    let mut results = Vec::with_capacity(jobs.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in jobs.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                chunk
                    .iter()
                    .map(|(index, path)| {
                        let started = Instant::now();
                        (*index, git::dirty_kinds(path), started.elapsed())
                    })
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            if let Ok(mut completed) = handle.join() {
                results.append(&mut completed);
            }
        }
    });

    for (index, dirty_kinds, elapsed) in results {
        let record = &mut records[index];
        if crate::proc::verbose() && elapsed.as_millis() >= 10 {
            eprintln!(
                "worktree status: {} ms ({})",
                elapsed.as_millis(),
                record.path
            );
        }
        record.dirty = !dirty_kinds.is_empty();
        record.dirty_kinds = dirty_kinds;
        record.action = recommend(record);
    }
}

fn enrich_filesystem_records(
    records: &mut [WorktreeRecord],
    state: &serde_json::Value,
    inactive_days: i64,
    dependency_dirs: &[String],
    now: f64,
    include_status: bool,
) {
    let jobs = records
        .iter()
        .enumerate()
        .filter(|(_, record)| !record.missing)
        .map(|(index, record)| {
            (
                index,
                PathBuf::from(&record.path),
                record.commit_ts,
                get_worktree_state(state, &record.path),
            )
        })
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return;
    }

    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(jobs.len())
        .min(16);
    let chunk_size = jobs.len().div_ceil(workers);
    let mut results = Vec::with_capacity(jobs.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in jobs.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                chunk
                    .iter()
                    .map(|(index, path, commit_ts, wt_state)| {
                        let dirty_kinds = if include_status {
                            git::dirty_kinds(path)
                        } else {
                            Vec::new()
                        };
                        let dep_scan =
                            sizes::find_dependency_dirs(path, dependency_dirs, DEP_BUDGET_S);
                        let threshold = inactive_days as f64 * 86400.0;
                        let walk = if commit_ts.is_none() || now - commit_ts.unwrap() > threshold {
                            Some(sizes::WalkResult {
                                max_mtime: dep_scan.activity_max_mtime,
                                incomplete: dep_scan.incomplete,
                                ..sizes::WalkResult::default()
                            })
                        } else {
                            None
                        };
                        let inactivity = activity::compute_inactivity_from_walk(
                            *commit_ts,
                            wt_state.as_ref(),
                            inactive_days,
                            now,
                            walk.as_ref(),
                        );
                        (*index, dirty_kinds, dep_scan, inactivity)
                    })
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            if let Ok(mut completed) = handle.join() {
                results.append(&mut completed);
            }
        }
    });

    for (index, dirty_kinds, dep_scan, inactivity) in results {
        let record = &mut records[index];
        record.dirty = !dirty_kinds.is_empty();
        record.dirty_kinds = dirty_kinds;
        record.sizes = SizeInfo {
            total_bytes: dep_scan.total_bytes,
            complete: !dep_scan.incomplete,
            reclaimable_bytes: dep_scan
                .targets
                .iter()
                .map(|target| target.size_bytes)
                .sum(),
            dep_dirs: dep_scan
                .targets
                .into_iter()
                .map(|target| DepDir {
                    path: target.path.to_string_lossy().into_owned(),
                    name: target.name,
                    size_bytes: target.size_bytes,
                    is_symlink: target.is_symlink,
                })
                .collect(),
        };
        record.inactivity = inactivity;
        record.inactive = record.inactivity.seconds >= inactive_days as f64 * 86400.0;
        record.action = recommend(record);
    }
}

pub fn recommend(record: &WorktreeRecord) -> String {
    if record.is_main {
        return KEEP_ACTIVE.into();
    }
    if record.missing {
        return MISSING.into();
    }
    if record.locked {
        return BLOCKED_LOCKED.into();
    }
    if record.detached {
        return BLOCKED_DETACHED.into();
    }
    if record.in_progress_op.is_some() {
        return BLOCKED_DIRTY.into();
    }
    if record.integration.state == INTEGRATED {
        if record.dirty {
            return BLOCKED_DIRTY.into();
        }
        if record.default_branch.is_some() {
            return REMOVE_MERGED.into();
        }
        return UNKNOWN.into();
    }
    if record.integration.state == INTEGRATION_UNKNOWN {
        return UNKNOWN.into();
    }
    if record.inactive {
        if !record.sizes.dep_dirs.is_empty() {
            return PURGE_DEPS.into();
        }
        return REVIEW_STALE.into();
    }
    KEEP_ACTIVE.into()
}
