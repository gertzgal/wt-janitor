use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub const STATE_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct WorktreeState {
    pub first_seen: f64,
    pub last_used: Option<f64>,
    pub last_used_source: Option<String>,
}

pub fn load_state(path: &Path) -> Value {
    if !path.is_file() {
        return Value::Object(Map::new());
    }
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Value::Object(map),
            _ => Value::Object(Map::new()),
        },
        Err(_) => Value::Object(Map::new()),
    }
}

pub fn get_worktree_state(state: &Value, worktree_path: &str) -> Option<WorktreeState> {
    let entry = state
        .get("worktrees")?
        .as_object()?
        .get(worktree_path)?
        .as_object()?;
    let first_seen = entry.get("first_seen").and_then(json_f64)?;
    let last_used = entry.get("last_used").and_then(json_f64);
    let last_used_source = entry
        .get("last_used_source")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(WorktreeState {
        first_seen,
        last_used,
        last_used_source,
    })
}

fn json_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

pub fn record_discovery(state: &mut Value, worktree_paths: &[String], now: f64) {
    let obj = state.as_object_mut().expect("state is object");
    let worktrees = obj
        .entry("worktrees")
        .or_insert_with(|| Value::Object(Map::new()));
    let map = worktrees.as_object_mut().expect("worktrees is object");
    for wt in worktree_paths {
        let needs_insert = match map.get(wt) {
            Some(Value::Object(e)) => !e.contains_key("first_seen"),
            _ => true,
        };
        if needs_insert {
            let mut entry = Map::new();
            entry.insert("first_seen".into(), json_num(now));
            map.insert(wt.clone(), Value::Object(entry));
        }
    }
}

pub fn record_touch(state: &mut Value, worktree_path: &str, now: f64) {
    let obj = state.as_object_mut().expect("state is object");
    let worktrees = obj
        .entry("worktrees")
        .or_insert_with(|| Value::Object(Map::new()));
    let map = worktrees.as_object_mut().expect("worktrees is object");
    let entry = match map.get_mut(worktree_path) {
        Some(Value::Object(_)) => map.get_mut(worktree_path).unwrap(),
        _ => {
            let mut fresh = Map::new();
            fresh.insert("first_seen".into(), json_num(now));
            map.insert(worktree_path.to_string(), Value::Object(fresh));
            map.get_mut(worktree_path).unwrap()
        }
    };
    let obj = entry.as_object_mut().unwrap();
    obj.insert("last_used".into(), json_num(now));
    obj.insert("last_used_source".into(), Value::String("touch".into()));
}

fn json_num(n: f64) -> Value {
    serde_json::Number::from_f64(n)
        .map(Value::Number)
        .unwrap_or(Value::Number(0.into()))
}

pub fn atomic_write_json(path: &Path, data: &Value) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let payload = format!(
        "{}\n",
        serde_json::to_string_pretty(data)
            .map_err(|e| { Error::operational(format!("failed to serialize state: {e}")) })?
    );

    let tmp_name = format!(
        "{}.{}.{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("state.json"),
        std::process::id(),
        unique_suffix()
    );
    let tmp_path = parent.join(tmp_name);

    let write_result = (|| -> Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        let mut fh = opts.open(&tmp_path)?;
        fh.write_all(payload.as_bytes())?;
        fh.flush()?;
        fh.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
        return write_result.map_err(|e| {
            Error::operational(format!(
                "failed to write state file {}: {e}",
                path.display()
            ))
        });
    }

    if let Ok(dir) = File::open(parent) {
        let _ = unsafe { libc::fsync(dir.as_raw_fd()) };
    }
    Ok(())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn persist_state(path: &Path, mutate: impl FnOnce(&mut Value)) -> Result<Value> {
    let mut state = load_state(path);
    mutate(&mut state);
    atomic_write_json(path, &state)?;
    Ok(state)
}
