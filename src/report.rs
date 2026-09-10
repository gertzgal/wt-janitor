//! Human-readable terminal output and stable JSON payloads.
//!
//! Rendering never fails the program: everything is best-effort printing to
//! stdout (reports) or stderr (operational errors). The JSON builders are pure
//! functions so the CLI can serialize without touching a terminal.

use std::io::{IsTerminal, Write};

use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use serde_json::{json, Map, Value};

use crate::cleanup::{DepPlan, MergePlan};
use crate::discovery::RepoScan;
use crate::doctor::DoctorReport;
use crate::models::{
    WorktreeRecord, BLOCKED_DETACHED, BLOCKED_DIRTY, BLOCKED_LOCKED, KEEP_ACTIVE, MISSING,
    PURGE_DEPS, REMOVE_MERGED, REVIEW_STALE, UNKNOWN,
};
use crate::sizes::human_bytes;

pub const DRY_RUN_BANNER: &str =
    "DRY RUN — no files or worktrees will be removed.\nRe-run with --apply to execute this plan.";

/// Fallback width used when stdout is not a terminal (matches the Python port).
pub const PIPED_WIDTH: usize = 200;

// ---------------------------------------------------------------------------
// terminal plumbing
// ---------------------------------------------------------------------------

/// Console width: real terminal width when attached to a tty, else `$COLUMNS`
/// or a wide default so piped output is not wrapped at 80 columns.
pub fn console_width() -> usize {
    if std::io::stdout().is_terminal() {
        if let Some(w) = tty_width() {
            return w;
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(PIPED_WIDTH)
}

fn tty_width() -> Option<usize> {
    // SAFETY: `winsize` is plain data and the ioctl only writes into it.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            Some(ws.ws_col as usize)
        } else {
            None
        }
    }
}

/// Print a line to stdout, ignoring broken pipes.
pub fn out(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print a line to stderr, ignoring broken pipes.
pub fn errln(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Serialize a JSON payload to stdout with two-space indentation.
pub fn print_json(payload: &Value) {
    match serde_json::to_string_pretty(payload) {
        Ok(text) => out(&text),
        Err(err) => errln(&format!("error: could not serialize JSON output: {err}")),
    }
}

// ---------------------------------------------------------------------------
// small text helpers
// ---------------------------------------------------------------------------

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Truncate to `max` characters, marking elision with a single ellipsis.
pub fn ellipsis(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if char_len(text) <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

/// Replace the home prefix with `~/`. Empty paths render as `-` (a pruned
/// worktree directory can already be gone).
pub fn short_path(path: Option<&str>, home: &str) -> String {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return "-".to_string();
    };
    let prefix = home.trim_end_matches('/');
    if prefix.is_empty() {
        return path.to_string();
    }
    let with_sep = format!("{prefix}/");
    if let Some(rest) = path.strip_prefix(&with_sep) {
        return format!("~/{rest}");
    }
    path.to_string()
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `+1,234` / `-1,234` — signed, thousands-separated (Python's `{:+,}`).
pub fn signed_thousands(n: i64) -> String {
    let sign = if n < 0 { '-' } else { '+' };
    format!("{sign}{}", thousands(n.unsigned_abs()))
}

// ---------------------------------------------------------------------------
// rules, panels
// ---------------------------------------------------------------------------

/// A full-width horizontal rule with an embedded title.
pub fn rule(title: &str) {
    let width = console_width().clamp(20, 200);
    let label = format!(" {title} ");
    let label_len = char_len(&label);
    if label_len + 4 >= width {
        out(&label);
        return;
    }
    let remaining = width - label_len;
    let left = remaining / 2;
    let right = remaining - left;
    out(&format!("{}{label}{}", "─".repeat(left), "─".repeat(right)));
}

/// Border characters for [`panel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    Single,
    Double,
}

impl Border {
    fn chars(self) -> (char, char, char, char, char, char) {
        match self {
            // horizontal, vertical, top-left, top-right, bottom-left, bottom-right
            Self::Single => ('─', '│', '╭', '╮', '╰', '╯'),
            Self::Double => ('═', '║', '╔', '╗', '╚', '╝'),
        }
    }
}

/// Render a titled box around `body`, sized to its content.
pub fn panel(title: &str, body: &str, border: Border) {
    let (h, v, tl, tr, bl, br) = border.chars();
    let lines: Vec<&str> = body.lines().collect();
    let title_label = if title.is_empty() {
        String::new()
    } else {
        format!(" {title} ")
    };
    let content_width = lines.iter().map(|l| char_len(l)).max().unwrap_or(0);
    let inner = content_width
        .max(char_len(&title_label))
        .min(console_width().saturating_sub(4).max(10));

    let mut top = String::new();
    top.push(tl);
    if title_label.is_empty() {
        top.push_str(&h.to_string().repeat(inner + 2));
    } else {
        top.push(h);
        top.push_str(&title_label);
        let used = 1 + char_len(&title_label);
        top.push_str(&h.to_string().repeat((inner + 2).saturating_sub(used)));
    }
    top.push(tr);
    out(&top);

    for line in lines {
        let line = ellipsis(line, inner);
        let pad = inner.saturating_sub(char_len(&line));
        out(&format!("{v} {line}{} {v}", " ".repeat(pad)));
    }

    out(&format!("{bl}{}{br}", h.to_string().repeat(inner + 2)));
}

pub fn print_dry_run_banner() {
    panel("dry run", DRY_RUN_BANNER, Border::Double);
}

fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Disabled);
    table
}

fn align_right(table: &mut Table, columns: &[usize]) {
    for idx in columns {
        if let Some(column) = table.column_mut(*idx) {
            column.set_cell_alignment(CellAlignment::Right);
        }
    }
}

// ---------------------------------------------------------------------------
// scan
// ---------------------------------------------------------------------------

/// Terminal colour for a recommended action.
pub fn action_color(action: &str) -> Color {
    match action {
        REMOVE_MERGED => Color::Cyan,
        PURGE_DEPS => Color::Yellow,
        REVIEW_STALE => Color::Magenta,
        KEEP_ACTIVE => Color::Green,
        BLOCKED_DIRTY | BLOCKED_LOCKED | BLOCKED_DETACHED | UNKNOWN => Color::Red,
        MISSING => Color::DarkGrey,
        _ => Color::White,
    }
}

fn state_markers(rec: &WorktreeRecord) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if rec.is_main {
        parts.push("MAIN");
    }
    if rec.is_current {
        parts.push("current");
    }
    if rec.locked {
        parts.push("locked");
    }
    if rec.detached {
        parts.push("detached");
    }
    if !rec.is_main && parts.is_empty() {
        parts.push("linked");
    }
    parts.join(" ")
}

fn dirty_marker(rec: &WorktreeRecord) -> String {
    if !rec.dirty {
        return "-".to_string();
    }
    rec.dirty_kinds.join(",")
}

fn ahead_behind(ahead: Option<i64>, behind: Option<i64>) -> String {
    if ahead.is_none() && behind.is_none() {
        return "-".to_string();
    }
    let mut parts = Vec::new();
    if let Some(a) = ahead.filter(|a| *a != 0) {
        parts.push(format!("↑{a}"));
    }
    if let Some(b) = behind.filter(|b| *b != 0) {
        parts.push(format!("↓{b}"));
    }
    if parts.is_empty() {
        "=".to_string()
    } else {
        parts.join(" ")
    }
}

fn inactivity_cell(rec: &WorktreeRecord, now: f64) -> String {
    let mut text = format!("{} ({})", rec.inactivity.age(now), rec.inactivity.source);
    if rec.inactivity.incomplete {
        text.push_str(" ⚠incomplete");
    }
    text
}

/// Render a scan across repositories as one table plus a summary panel.
pub fn render_scan_table(scans: &[RepoScan], inactive_days: i64, now: f64) {
    let home = home();
    out(&format!(
        "wt-janitor scan — inactive threshold: {inactive_days}d"
    ));

    let mut table = new_table();
    table.set_header(vec![
        Cell::new("Repo").add_attribute(Attribute::Bold),
        Cell::new("Branch").add_attribute(Attribute::Bold),
        Cell::new("Worktree path").add_attribute(Attribute::Bold),
        Cell::new("HEAD").add_attribute(Attribute::Bold),
        Cell::new("Inactive").add_attribute(Attribute::Bold),
        Cell::new("State").add_attribute(Attribute::Bold),
        Cell::new("Dirty").add_attribute(Attribute::Bold),
        Cell::new("main↕").add_attribute(Attribute::Bold),
        Cell::new("remote⇅").add_attribute(Attribute::Bold),
        Cell::new("Integrated").add_attribute(Attribute::Bold),
        Cell::new("Reason").add_attribute(Attribute::Bold),
        Cell::new("Size").add_attribute(Attribute::Bold),
        Cell::new("Reclaim").add_attribute(Attribute::Bold),
        Cell::new("Action").add_attribute(Attribute::Bold),
    ]);
    align_right(&mut table, &[7, 8, 11, 12]);

    for scan in scans {
        for rec in &scan.worktrees {
            let size = format!(
                "{}{}",
                human_bytes(rec.sizes.total_bytes),
                if rec.sizes.complete { "" } else { " ⚠" }
            );
            table.add_row(vec![
                Cell::new(&rec.repo).add_attribute(Attribute::Bold),
                Cell::new(ellipsis(rec.branch.as_deref().unwrap_or("(detached)"), 34))
                    .fg(Color::Cyan),
                Cell::new(ellipsis(&short_path(Some(&rec.path), &home), 44)),
                Cell::new(ellipsis(rec.short_sha.as_deref().unwrap_or("-"), 10))
                    .add_attribute(Attribute::Dim),
                Cell::new(ellipsis(&inactivity_cell(rec, now), 26)),
                Cell::new(ellipsis(&state_markers(rec), 18)),
                Cell::new(ellipsis(&dirty_marker(rec), 22)),
                Cell::new(ahead_behind(rec.main_ahead, rec.main_behind)),
                Cell::new(ahead_behind(rec.remote_ahead, rec.remote_behind)),
                Cell::new(ellipsis(&rec.integration.state, 14)),
                Cell::new(ellipsis(
                    rec.integration.reason.as_deref().unwrap_or("-"),
                    22,
                )),
                Cell::new(size),
                Cell::new(human_bytes(rec.sizes.reclaimable_bytes)),
                Cell::new(&rec.action)
                    .fg(action_color(&rec.action))
                    .add_attribute(Attribute::Bold),
            ]);
        }
        for err in &scan.errors {
            let mut row = vec![
                Cell::new(&scan.repo_name).add_attribute(Attribute::Bold),
                Cell::new("error").fg(Color::Red),
                Cell::new(ellipsis(err, 80)),
            ];
            row.extend((0..11).map(|_| Cell::new("")));
            table.add_row(row);
        }
    }
    out(&table.to_string());
    print_scan_summary(scans);
}

fn print_scan_summary(scans: &[RepoScan]) {
    let mut counts: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    let mut total_reclaim = 0u64;
    let mut total_size = 0u64;
    let mut worktree_count = 0u64;
    for scan in scans {
        for rec in &scan.worktrees {
            if !rec.is_main {
                worktree_count += 1;
            }
            *counts.entry(rec.action.as_str()).or_insert(0) += 1;
            total_reclaim += rec.sizes.reclaimable_bytes;
            total_size += rec.sizes.total_bytes;
        }
    }
    let mut lines = vec![format!(
        "{worktree_count} linked worktrees across {} repositories",
        scans.len()
    )];
    for (action, n) in &counts {
        lines.push(format!("  {action}: {n}"));
    }
    lines.push(format!("Total logical size: {}", human_bytes(total_size)));
    lines.push(format!(
        "Estimated reclaimable (dependency dirs in inactive worktrees): {}",
        human_bytes(total_reclaim)
    ));
    panel("summary", &lines.join("\n"), Border::Single);
}

/// Stable machine-readable structure: `wt-janitor.scan/1`.
pub fn scan_to_json(scans: &[RepoScan], inactive_days: i64, mode: &str, now: f64) -> Value {
    let mut repos = Vec::with_capacity(scans.len());
    for scan in scans {
        repos.push(json!({
            "name": scan.repo_name,
            "path": scan.repo_path,
            "default_branch": scan.default_branch,
            "is_main_worktree": scan.is_main,
            "fetch": {
                "requested": scan.fetch_ok.is_some(),
                "ok": scan.fetch_ok,
                "integration_stale": scan.fetch_stale,
            },
            "errors": scan.errors,
            "worktrees": scan
                .worktrees
                .iter()
                .map(|rec| worktree_json(rec, now))
                .collect::<Vec<_>>(),
        }));
    }

    let mut by_action: Map<String, Value> = Map::new();
    let mut reclaim = 0u64;
    // Deterministic ordering: count into a sorted map first.
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for scan in scans {
        for rec in &scan.worktrees {
            *counts.entry(rec.action.clone()).or_insert(0) += 1;
            reclaim += rec.sizes.reclaimable_bytes;
        }
    }
    for (action, n) in counts {
        by_action.insert(action, json!(n));
    }

    json!({
        "schema": "wt-janitor.scan/1",
        "generated_at": iso_utc(now),
        "mode": mode,
        "inactive_days": inactive_days,
        "repos": repos,
        "summary": {
            "worktrees_by_action": Value::Object(by_action),
            "reclaimable_bytes": reclaim,
        },
    })
}

fn worktree_json(rec: &WorktreeRecord, now: f64) -> Value {
    let upstream = rec.remote_name.as_ref().map(|name| {
        json!({
            "name": name,
            "ahead": rec.remote_ahead,
            "behind": rec.remote_behind,
        })
    });
    json!({
        "repo": rec.repo,
        "path": rec.path,
        "branch": rec.branch,
        "head": rec.short_sha,
        "head_sha": rec.head_sha,
        "commit_message": rec.commit_message,
        "state": {
            "main": rec.is_main,
            "current": rec.is_current,
            "locked": rec.locked,
            "detached": rec.detached,
            "missing": rec.missing,
            "in_progress_op": rec.in_progress_op,
            "worktree_state_note": rec.worktree_state_note,
        },
        "dirty": rec.dirty,
        "dirty_kinds": rec.dirty_kinds,
        "default_branch": rec.default_branch,
        "main_ahead": rec.main_ahead,
        "main_behind": rec.main_behind,
        "upstream": upstream,
        "integration": {
            "state": rec.integration.state,
            "reason": rec.integration.reason,
        },
        "inactivity": {
            "seconds": rec.inactivity.seconds.round() as i64,
            "age": rec.inactivity.age(now),
            "source": rec.inactivity.source,
            "incomplete": rec.inactivity.incomplete,
        },
        "sizes": {
            "total_bytes": rec.sizes.total_bytes,
            "complete": rec.sizes.complete,
            "reclaimable_bytes": rec.sizes.reclaimable_bytes,
            "dependency_dirs": rec
                .sizes
                .dep_dirs
                .iter()
                .map(|d| json!({"path": d.path, "bytes": d.size_bytes, "symlink": d.is_symlink}))
                .collect::<Vec<_>>(),
        },
        "recommended_action": rec.action,
        "action_detail": rec.action_detail,
        "notes": rec.notes,
    })
}

// ---------------------------------------------------------------------------
// clean merged
// ---------------------------------------------------------------------------

/// Render merged-worktree plans (dry run) or apply results.
pub fn render_merged_plans(plans: &[MergePlan], apply: bool) {
    let home = home();
    for plan in plans {
        rule(&format!("{}: merged worktrees", plan.repo_name));

        if plan.candidates.is_empty() {
            out("No integrated worktree candidates.");
        } else {
            out("candidates for removal");
            let mut table = new_table();
            table.set_header(vec![
                Cell::new("Branch").add_attribute(Attribute::Bold),
                Cell::new("Path").add_attribute(Attribute::Bold),
                Cell::new("Integration reason").add_attribute(Attribute::Bold),
            ]);
            for c in &plan.candidates {
                table.add_row(vec![
                    Cell::new(ellipsis(c.branch.as_deref().unwrap_or("-"), 40)).fg(Color::Cyan),
                    Cell::new(ellipsis(&short_path(Some(&c.path), &home), 60)),
                    Cell::new(c.reason.as_deref().unwrap_or("-")),
                ]);
            }
            out(&table.to_string());
        }

        if !plan.blocked.is_empty() {
            out("integrated but blocked (skipped)");
            let mut table = new_table();
            table.set_header(vec![
                Cell::new("Branch").add_attribute(Attribute::Bold),
                Cell::new("Path").add_attribute(Attribute::Bold),
                Cell::new("Block reason").add_attribute(Attribute::Bold),
            ]);
            for c in &plan.blocked {
                table.add_row(vec![
                    Cell::new(ellipsis(c.branch.as_deref().unwrap_or("-"), 40)).fg(Color::Cyan),
                    Cell::new(ellipsis(&short_path(Some(&c.path), &home), 60)),
                    Cell::new(c.block_reason.as_deref().unwrap_or("-")).fg(Color::Red),
                ]);
            }
            out(&table.to_string());
        }

        if !plan.wt_candidates.is_empty() {
            out("removal queue (after min-age filter)");
            let mut table = new_table();
            table.set_header(vec![
                Cell::new("Branch").add_attribute(Attribute::Bold),
                Cell::new("Path").add_attribute(Attribute::Bold),
                Cell::new("Reason").add_attribute(Attribute::Bold),
                Cell::new("Target").add_attribute(Attribute::Bold),
            ]);
            for item in &plan.wt_candidates {
                table.add_row(vec![
                    Cell::new(ellipsis(json_str(item, "branch").unwrap_or("-"), 40))
                        .fg(Color::Cyan),
                    Cell::new(ellipsis(&short_path(json_str(item, "path"), &home), 60)),
                    Cell::new(ellipsis(json_str(item, "reason").unwrap_or("-"), 24)),
                    Cell::new(json_str(item, "target").unwrap_or("-")),
                ]);
            }
            out(&table.to_string());
        }

        for err in &plan.errors {
            errln(&format!("error ({}): {err}", plan.repo_name));
        }

        if apply {
            render_merge_apply_results(plan);
        } else {
            print_dry_run_banner();
        }
    }
}

fn json_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn render_merge_apply_results(plan: &MergePlan) {
    if plan.removed > 0 || plan.skipped > 0 || plan.failed > 0 {
        out(&format!(
            "Removed: {}  skipped: {}  failed: {}",
            plan.removed, plan.skipped, plan.failed
        ));
        for item in &plan.apply_results {
            out(&format!(
                "  • {} — {} ({})",
                json_str(item, "branch").unwrap_or("(no branch)"),
                json_str(item, "path").unwrap_or("(missing dir)"),
                json_str(item, "reason").unwrap_or("integrated"),
            ));
        }
    } else if plan.errors.is_empty() {
        out("Nothing was removed (no candidates passed the safety checks).");
    }
}

/// Stable machine-readable structure: `wt-janitor.clean-merged/1`.
pub fn merged_plans_json(plans: &[MergePlan], mode: &str) -> Value {
    let repos: Vec<Value> = plans
        .iter()
        .map(|p| {
            json!({
                "repo": p.repo_name,
                "errors": p.errors,
                "candidates": p
                    .candidates
                    .iter()
                    .map(|c| json!({
                        "repo": c.repo,
                        "path": c.path,
                        "branch": c.branch,
                        "reason": c.reason,
                    }))
                    .collect::<Vec<_>>(),
                "blocked": p
                    .blocked
                    .iter()
                    .map(|c| json!({
                        "repo": c.repo,
                        "path": c.path,
                        "branch": c.branch,
                        "reason": c.block_reason,
                    }))
                    .collect::<Vec<_>>(),
                "wt_prune_candidates": p.wt_candidates,
                "apply_results": p.apply_results,
                "removed": p.removed,
                "skipped": p.skipped,
                "failed": p.failed,
            })
        })
        .collect();
    json!({
        "schema": "wt-janitor.clean-merged/1",
        "mode": mode,
        "repos": repos,
    })
}

// ---------------------------------------------------------------------------
// clean deps
// ---------------------------------------------------------------------------

/// Render dependency-directory plans (dry run) or deletion results.
pub fn render_dep_plans(plans: &[DepPlan], apply: bool) {
    let home = home();
    for plan in plans {
        rule(&format!(
            "{}: dependency dirs in inactive worktrees",
            plan.repo_name
        ));
        for err in &plan.errors {
            errln(&format!("error ({}): {err}", plan.repo_name));
        }
        if plan.entries.is_empty() {
            out("No dependency directories found in inactive linked worktrees.");
            if !apply {
                print_dry_run_banner();
            }
            continue;
        }

        let mut table = new_table();
        table.set_header(vec![
            Cell::new("Worktree branch").add_attribute(Attribute::Bold),
            Cell::new("Target path").add_attribute(Attribute::Bold),
            Cell::new("Bytes").add_attribute(Attribute::Bold),
            Cell::new("Type").add_attribute(Attribute::Bold),
            Cell::new("Inactivity").add_attribute(Attribute::Bold),
        ]);
        align_right(&mut table, &[2]);
        for e in &plan.entries {
            table.add_row(vec![
                Cell::new(ellipsis(e.branch.as_deref().unwrap_or("-"), 34)).fg(Color::Cyan),
                Cell::new(ellipsis(&short_path(Some(&e.target), &home), 70)),
                Cell::new(e.size_bytes),
                Cell::new(if e.is_symlink { "symlink" } else { "dir" }),
                Cell::new(ellipsis(&e.stale_at_plan, 30)),
            ]);
        }
        out(&table.to_string());

        let total: u64 = plan.entries.iter().map(|e| e.size_bytes).sum();
        out(&format!(
            "Estimated logical reclaim: {} (logical estimate; APFS clone/reflink overhead may change actual reclaimed space)",
            human_bytes(total)
        ));

        if !apply {
            print_dry_run_banner();
            continue;
        }

        if !plan.deletions.is_empty() {
            out("deletion results");
            let mut table = new_table();
            table.set_header(vec![
                Cell::new("Path").add_attribute(Attribute::Bold),
                Cell::new("Result").add_attribute(Attribute::Bold),
            ]);
            for (path, status) in &plan.deletions {
                let color = if status == "deleted" {
                    Color::Green
                } else {
                    Color::Red
                };
                table.add_row(vec![
                    Cell::new(ellipsis(&short_path(Some(path), &home), 70)),
                    Cell::new(status).fg(color),
                ]);
            }
            out(&table.to_string());
        }

        if let Some(delta) = plan.free_delta {
            let body = format!(
                "Sum of deleted logical sizes: {}\nActual change in free filesystem space: {} bytes ({:+.1} MB)",
                human_bytes(plan.deleted_bytes),
                signed_thousands(delta),
                delta as f64 / 1024.0 / 1024.0,
            );
            panel("disk impact", &body, Border::Single);
        }

        if plan.skipped_reactivated > 0 {
            errln(&format!(
                "{} target(s) skipped — worktree became active again",
                plan.skipped_reactivated
            ));
        }
    }
}

/// Stable machine-readable structure: `wt-janitor.clean-deps/1`.
pub fn dep_plans_json(plans: &[DepPlan], mode: &str) -> Value {
    let repos: Vec<Value> = plans
        .iter()
        .map(|p| {
            json!({
                "repo": p.repo_name,
                "errors": p.errors,
                "targets": p
                    .entries
                    .iter()
                    .map(|e| json!({
                        "repo": e.repo,
                        "worktree": e.worktree,
                        "branch": e.branch,
                        "path": e.target,
                        "name": e.name,
                        "bytes": e.size_bytes,
                        "symlink": e.is_symlink,
                        "inactivity": e.stale_at_plan,
                    }))
                    .collect::<Vec<_>>(),
                "deletions": p
                    .deletions
                    .iter()
                    .map(|(path, status)| json!({"path": path, "status": status}))
                    .collect::<Vec<_>>(),
                "deleted_bytes": p.deleted_bytes,
                "free_space_delta_bytes": p.free_delta,
            })
        })
        .collect();
    json!({
        "schema": "wt-janitor.clean-deps/1",
        "mode": mode,
        "repos": repos,
    })
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

fn status_icon(status: &str) -> Cell {
    match status {
        "ok" => Cell::new("✓").fg(Color::Green),
        "fail" => Cell::new("✗").fg(Color::Red),
        "warn" => Cell::new("!").fg(Color::Yellow),
        "info" => Cell::new("i").add_attribute(Attribute::Dim),
        other => Cell::new(other),
    }
}

/// Render the doctor checks as a table.
pub fn render_doctor(report: &DoctorReport) {
    out("wt-janitor doctor");
    let mut table = new_table();
    table.set_header(vec![
        Cell::new("Check").add_attribute(Attribute::Bold),
        Cell::new("Status").add_attribute(Attribute::Bold),
        Cell::new("Detail").add_attribute(Attribute::Bold),
    ]);
    align_right(&mut table, &[]);
    if let Some(column) = table.column_mut(1) {
        column.set_cell_alignment(CellAlignment::Center);
    }
    for check in &report.checks {
        table.add_row(vec![
            Cell::new(&check.name),
            status_icon(check.status.as_str()),
            Cell::new(ellipsis(&check.detail, 120)),
        ]);
    }
    out(&table.to_string());
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// Trailing note pointing at operational errors already printed above.
pub fn render_errors_summary(error_count: usize) {
    if error_count > 0 {
        errln(&format!("{error_count} operational error(s) — see above."));
    }
}

// ---------------------------------------------------------------------------
// time formatting
// ---------------------------------------------------------------------------

/// `YYYY-MM-DDTHH:MM:SSZ` for a UNIX timestamp (UTC), without pulling in chrono.
pub fn iso_utc(ts: f64) -> String {
    let secs = ts as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_formats_epoch_and_a_known_instant() {
        assert_eq!(iso_utc(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_757_464_872.0), "2025-09-10T00:41:12Z");
    }

    #[test]
    fn short_path_swaps_home_prefix_and_handles_missing() {
        assert_eq!(short_path(Some("/home/me/x"), "/home/me"), "~/x");
        assert_eq!(short_path(Some("/other/x"), "/home/me"), "/other/x");
        assert_eq!(short_path(None, "/home/me"), "-");
        assert_eq!(short_path(Some(""), "/home/me"), "-");
    }

    #[test]
    fn ellipsis_truncates_on_char_boundaries() {
        assert_eq!(ellipsis("abcdef", 6), "abcdef");
        assert_eq!(ellipsis("abcdef", 4), "abc…");
        assert_eq!(ellipsis("äöüßx", 3), "äö…");
    }

    #[test]
    fn ahead_behind_renders_arrows() {
        assert_eq!(ahead_behind(None, None), "-");
        assert_eq!(ahead_behind(Some(0), Some(0)), "=");
        assert_eq!(ahead_behind(Some(2), Some(0)), "↑2");
        assert_eq!(ahead_behind(Some(2), Some(3)), "↑2 ↓3");
    }

    #[test]
    fn signed_thousands_matches_python_format() {
        assert_eq!(signed_thousands(0), "+0");
        assert_eq!(signed_thousands(1_234_567), "+1,234,567");
        assert_eq!(signed_thousands(-4_096), "-4,096");
    }
}
