use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn default_config_path() -> PathBuf {
    home_dir().join(".config/wt-janitor/config.toml")
}

pub fn default_state_path() -> PathBuf {
    home_dir().join(".local/state/wt-janitor/state.json")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub const DEFAULT_DEPENDENCY_DIRS: [&str; 2] = ["node_modules", ".venv"];
pub const DEFAULT_INACTIVE_DAYS: i64 = 7;

pub const EXAMPLE_CONFIG: &str = r#"# wt-janitor configuration
# Docs: see README.md

# Worktrees with no activity for this many days are considered inactive.
inactive_days = 7

[cleanup]
# Exact directory names that `wt-janitor clean deps` may remove from
# inactive linked worktrees. Keep this list conservative: only reproducible
# directories that are safe to re-create (e.g. via `pnpm install`, `uv sync`).
dependency_dirs = ["node_modules", ".venv"]

# Optional: separate minimum age (in days) for bulk worktree removals.
# Defaults to the same threshold as `inactive_days`.
# prune_min_age_days = 7

# Repositories whose registered worktrees should be managed.
# Paths support ~ expansion. wt-janitor never creates worktrees.
[[repos]]
name = "web"
path = "~/Projects/web"

[[repos]]
name = "api"
path = "~/Projects/api"
"#;

#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub name: String,
    pub raw_path: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub inactive_days: i64,
    pub dependency_dirs: Vec<String>,
    pub prune_min_age_days: Option<i64>,
    pub repos: Vec<RepoConfig>,
    pub path: PathBuf,
}

pub fn load_config(path: &Path) -> Result<Config> {
    if !path.is_file() {
        return Err(Error::config(format!(
            "configuration file not found: {}\nRun 'wt-janitor init' to create an example configuration.",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::config(format!("could not read {}: {e}", path.display())))?;
    let raw: toml::Value = toml::from_str(&text)
        .map_err(|e| Error::config(format!("invalid TOML in {}: {e}", path.display())))?;

    let table = raw
        .as_table()
        .ok_or_else(|| Error::config("configuration must be a table"))?;

    let inactive_days = match table.get("inactive_days") {
        None => DEFAULT_INACTIVE_DAYS,
        Some(toml::Value::Integer(n)) if *n >= 1 => *n,
        Some(_) => return Err(Error::config("'inactive_days' must be an integer >= 1")),
    };

    let cleanup = match table.get("cleanup") {
        None => None,
        Some(toml::Value::Table(t)) => Some(t),
        Some(_) => return Err(Error::config("'[cleanup]' must be a table")),
    };

    let dependency_dirs = match cleanup.and_then(|c| c.get("dependency_dirs")) {
        None => DEFAULT_DEPENDENCY_DIRS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        Some(v) => validate_dep_dirs(v)?,
    };

    let prune_min_age_days = match cleanup.and_then(|c| c.get("prune_min_age_days")) {
        None => None,
        Some(toml::Value::Integer(n)) if *n >= 0 => Some(*n),
        Some(_) => {
            return Err(Error::config(
                "'prune_min_age_days' must be an integer >= 0",
            ));
        }
    };

    let repos_raw = match table.get("repos") {
        None => Vec::new(),
        Some(toml::Value::Array(a)) => a.clone(),
        Some(_) => return Err(Error::config("'[[repos]]' must be an array of tables")),
    };

    let mut repos = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    for (i, item) in repos_raw.iter().enumerate() {
        let Some(tbl) = item.as_table() else {
            return Err(Error::config(format!(
                "repos[{i}] must be a table with 'name' and 'path'"
            )));
        };
        let name = match tbl.get("name") {
            Some(toml::Value::String(s)) if !s.trim().is_empty() => s.clone(),
            _ => {
                return Err(Error::config(format!(
                    "repos[{i}].name must be a non-empty string"
                )));
            }
        };
        let raw_path = match tbl.get("path") {
            Some(toml::Value::String(s)) if !s.trim().is_empty() => s.clone(),
            _ => {
                return Err(Error::config(format!(
                    "repos[{i}].path must be a non-empty string"
                )));
            }
        };
        if !seen_names.insert(name.clone()) {
            return Err(Error::config(format!(
                "duplicate repository name: '{name}'"
            )));
        }
        let path = expand_user(&raw_path);
        repos.push(RepoConfig {
            name,
            raw_path,
            path,
        });
    }

    Ok(Config {
        inactive_days,
        dependency_dirs,
        prune_min_age_days,
        repos,
        path: path.to_path_buf(),
    })
}

fn validate_dep_dirs(value: &toml::Value) -> Result<Vec<String>> {
    let Some(arr) = value.as_array() else {
        return Err(Error::config(
            "'cleanup.dependency_dirs' must be an array of strings",
        ));
    };
    let mut out = Vec::new();
    for d in arr {
        let Some(s) = d.as_str() else {
            return Err(Error::config(
                "'cleanup.dependency_dirs' must be an array of strings",
            ));
        };
        if s.is_empty()
            || s == "."
            || s == ".."
            || s.contains('/')
            || s.contains('\\')
            || s != s.trim()
        {
            return Err(Error::config(format!(
                "cleanup.dependency_dirs entries must be simple directory names, got '{s}'"
            )));
        }
        out.push(s.to_string());
    }
    Ok(out)
}

pub fn expand_user(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    if raw == "~" {
        return home_dir();
    }
    PathBuf::from(raw)
}

pub fn select_repos<'a>(config: &'a Config, wanted: &[String]) -> Result<Vec<&'a RepoConfig>> {
    if wanted.is_empty() {
        return Ok(config.repos.iter().collect());
    }
    let mut unknown = Vec::new();
    let mut out = Vec::new();
    for w in wanted {
        match config.repos.iter().find(|r| r.name == *w) {
            Some(r) => out.push(r),
            None => unknown.push(w.as_str()),
        }
    }
    if !unknown.is_empty() {
        let configured = if config.repos.is_empty() {
            "(none)".into()
        } else {
            config
                .repos
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(Error::config(format!(
            "unknown repository name(s): {} — configured: {configured}",
            unknown.join(", ")
        )));
    }
    Ok(out)
}

pub fn write_example_config(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, EXAMPLE_CONFIG)?;
    Ok(true)
}

pub fn prune_min_age_days(config: &Config) -> i64 {
    config.prune_min_age_days.unwrap_or(config.inactive_days)
}
