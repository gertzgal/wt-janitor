use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::cleanup;
use crate::config::{self, Config};
use crate::discovery;
use crate::error::{Result, EXIT_OK, EXIT_OPERATIONAL, EXIT_SAFETY};
use crate::{doctor, git, proc, progress::Progress, report, state};

#[derive(Parser)]
#[command(name = "wt-janitor", version, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    Doctor(DoctorArgs),
    Scan(ScanArgs),
    #[command(subcommand)]
    Clean(CleanCommand),
    Touch(TouchArgs),
}

#[derive(Subcommand)]
enum CleanCommand {
    Merged(MergedArgs),
    Deps(DepsArgs),
}

#[derive(Args)]
struct BaseArgs {
    #[arg(long = "repo")]
    repos: Vec<String>,
    #[arg(long)]
    inactive_days: Option<i64>,
    #[arg(long)]
    json: bool,
    #[arg(short = 'v', long)]
    verbose: bool,
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Args)]
struct InitArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args)]
struct ScanArgs {
    #[command(flatten)]
    base: BaseArgs,
    #[arg(long)]
    fetch: bool,
}

#[derive(Args)]
struct MergedArgs {
    #[command(flatten)]
    base: BaseArgs,
    #[arg(long)]
    fetch: bool,
    #[arg(long)]
    apply: bool,
}

#[derive(Args)]
struct DepsArgs {
    #[command(flatten)]
    base: BaseArgs,
    #[arg(long)]
    apply: bool,
}

#[derive(Args)]
struct TouchArgs {
    path: Option<PathBuf>,
    #[arg(long = "repo")]
    repos: Vec<String>,
    #[arg(short = 'v', long)]
    verbose: bool,
    #[arg(long)]
    config: Option<PathBuf>,
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init(args),
        Command::Doctor(args) => Ok(doctor_command(args)),
        Command::Scan(args) => scan(args),
        Command::Clean(CleanCommand::Merged(args)) => clean_merged(args),
        Command::Clean(CleanCommand::Deps(args)) => clean_deps(args),
        Command::Touch(args) => touch(args),
    }
}

fn config_path(path: Option<PathBuf>) -> PathBuf {
    path.unwrap_or_else(config::default_config_path)
}

fn state_path() -> PathBuf {
    std::env::var_os("WT_JANITOR_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(config::default_state_path)
}

fn init(args: InitArgs) -> Result<i32> {
    proc::set_verbose(args.verbose);
    let path = config_path(args.config);
    let progress = Progress::new("Preparing configuration…");
    let created = config::write_example_config(&path)?;
    progress.finish("Configuration ready");
    if created {
        println!("Created example configuration: {}", path.display());
    } else {
        println!(
            "Configuration already exists — left untouched: {}",
            path.display()
        );
    }
    println!("Edit the file to point at your repositories, then run: wt-janitor doctor");
    Ok(EXIT_OK)
}

fn doctor_command(args: DoctorArgs) -> i32 {
    proc::set_verbose(args.verbose);
    let progress = Progress::new("Checking Git, configuration, repositories, and state…");
    let result = doctor::run_doctor_with_state(&config_path(args.config), &state_path());
    progress.finish("Doctor checks complete");
    if args.json {
        report::print_json(&result.to_json());
    } else {
        report::render_doctor(&result);
    }
    result.exit_code
}

fn load_and_select(path: Option<PathBuf>, wanted: &[String]) -> Result<(Config, Vec<usize>)> {
    let config = config::load_config(&config_path(path))?;
    let selected = config::select_repos(&config, wanted)?;
    let indices = selected
        .iter()
        .filter_map(|repo| {
            config
                .repos
                .iter()
                .position(|candidate| candidate.name == repo.name)
        })
        .collect();
    Ok((config, indices))
}

fn selected<'a>(config: &'a Config, indices: &[usize]) -> Vec<&'a config::RepoConfig> {
    indices.iter().map(|index| &config.repos[*index]).collect()
}

fn effective_days(value: Option<i64>, configured: i64) -> i64 {
    match value {
        Some(days) if days > 0 => days,
        _ => configured,
    }
}

fn scan(args: ScanArgs) -> Result<i32> {
    proc::set_verbose(args.base.verbose);
    let (config, indices) = load_and_select(args.base.config, &args.base.repos)?;
    let repos = selected(&config, &indices);
    let days = effective_days(args.base.inactive_days, config.inactive_days);
    let state_path = state_path();
    let loaded = state::load_state(&state_path);
    let now = discovery::now_secs();
    let mut scans = Vec::new();
    let mut failures = 0;
    let repo_count = repos.len();
    let progress = Progress::new(format!("Scanning {repo_count} repositories…"));
    for (index, repo) in repos.into_iter().enumerate() {
        progress.set_message(format!(
            "Scanning {} ({}/{repo_count}): discovering worktrees and integration…",
            repo.name,
            index + 1
        ));
        match discovery::discover_repo(
            repo,
            &loaded,
            days,
            &config.dependency_dirs,
            args.fetch,
            now,
        ) {
            Ok(scan) => scans.push(scan),
            Err(error) => {
                failures += 1;
                eprintln!("scan failed for {}: {error}", repo.name);
            }
        }
    }
    let paths = scans
        .iter()
        .flat_map(|scan| scan.worktrees.iter().map(|record| record.path.clone()))
        .collect::<Vec<_>>();
    progress.set_message("Saving discovery state…");
    if let Err(error) = state::persist_state(&state_path, |value| {
        state::record_discovery(value, &paths, now)
    }) {
        eprintln!("could not persist state: {error}");
    }
    progress.finish(format!(
        "Scan complete: {} worktrees across {} repositories",
        scans.iter().map(|scan| scan.worktrees.len()).sum::<usize>(),
        scans.len()
    ));
    if args.base.json {
        report::print_json(&report::scan_to_json(&scans, days, "scan", now));
    } else {
        report::render_scan_table(&scans, days, now);
    }
    Ok(if failures == 0 {
        EXIT_OK
    } else {
        EXIT_OPERATIONAL
    })
}

fn clean_merged(args: MergedArgs) -> Result<i32> {
    proc::set_verbose(args.base.verbose);
    let (mut config, indices) = load_and_select(args.base.config, &args.base.repos)?;
    config.inactive_days = effective_days(args.base.inactive_days, config.inactive_days);
    let repos = selected(&config, &indices);
    let loaded = state::load_state(&state_path());
    let mode = if args.apply { "Applying" } else { "Planning" };
    let progress = Progress::new(format!(
        "{mode} merged-worktree cleanup across {} repositories…",
        repos.len()
    ));
    let plans = if args.apply {
        cleanup::apply_merged(&config, &repos, &loaded, args.fetch)
    } else {
        cleanup::plan_merged(&config, &repos, &loaded, args.fetch)
    };
    progress.finish(format!(
        "Merged-worktree {} complete",
        if args.apply { "cleanup" } else { "plan" }
    ));
    if args.base.json {
        report::print_json(&report::merged_plans_json(
            &plans,
            if args.apply { "apply" } else { "dry-run" },
        ));
    } else {
        report::render_merged_plans(&plans, args.apply);
    }
    if !args.apply {
        return Ok(EXIT_OK);
    }
    if plans.iter().any(|plan| plan.safety_refused) {
        return Ok(EXIT_SAFETY);
    }
    Ok(if plans.iter().any(|plan| !plan.errors.is_empty()) {
        EXIT_OPERATIONAL
    } else {
        EXIT_OK
    })
}

fn clean_deps(args: DepsArgs) -> Result<i32> {
    proc::set_verbose(args.base.verbose);
    let (mut config, indices) = load_and_select(args.base.config, &args.base.repos)?;
    config.inactive_days = effective_days(args.base.inactive_days, config.inactive_days);
    let repos = selected(&config, &indices);
    let mode = if args.apply { "Applying" } else { "Planning" };
    let progress = Progress::new(format!(
        "{mode} dependency cleanup across {} repositories…",
        repos.len()
    ));
    let plans = if args.apply {
        cleanup::apply_deps(&config, &repos, &state_path(), None)
    } else {
        cleanup::plan_deps(&config, &repos, &state::load_state(&state_path()), None)
    };
    progress.finish(format!(
        "Dependency {} complete",
        if args.apply { "cleanup" } else { "plan" }
    ));
    if args.base.json {
        report::print_json(&report::dep_plans_json(
            &plans,
            if args.apply { "apply" } else { "dry-run" },
        ));
    } else {
        report::render_dep_plans(&plans, args.apply);
    }
    Ok(
        if args.apply && plans.iter().any(|plan| !plan.errors.is_empty()) {
            EXIT_OPERATIONAL
        } else {
            EXIT_OK
        },
    )
}

fn touch(args: TouchArgs) -> Result<i32> {
    proc::set_verbose(args.verbose);
    let (config, indices) = load_and_select(args.config, &args.repos)?;
    let progress = Progress::new("Locating containing worktree…");
    let target = args
        .path
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()
        .map_err(|e| crate::Error::usage(format!("cannot resolve path: {e}")))?;
    let mut matches = Vec::new();
    for repo in selected(&config, &indices) {
        let Ok(repo_path) = discovery::resolve_repo_path(repo) else {
            continue;
        };
        let Ok(entries) = git::worktree_list_porcelain(&repo_path) else {
            continue;
        };
        for entry in entries {
            let worktree = crate::sizes::realpath(&entry.path);
            if target == worktree || target.starts_with(&worktree) {
                matches.push((repo.name.clone(), worktree));
            }
        }
    }
    let Some((repo_name, worktree)) = matches
        .into_iter()
        .max_by_key(|(_, path)| path.components().count())
    else {
        return Err(crate::Error::usage(format!(
            "path is not inside a registered worktree of the configured repositories: {}",
            target.display()
        )));
    };
    let worktree_text = worktree.to_string_lossy().into_owned();
    progress.set_message("Recording explicit activity…");
    state::persist_state(&state_path(), |value| {
        state::record_touch(value, &worktree_text, discovery::now_secs())
    })?;
    progress.finish("Activity timestamp recorded");
    println!(
        "touched worktree {} (repo: {}) — explicit last-used timestamp recorded",
        worktree.display(),
        repo_name
    );
    Ok(EXIT_OK)
}
