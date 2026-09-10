use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::{Error, Result};

static VERBOSE: AtomicBool = AtomicBool::new(false);

const SECRET_URL: &str = r"(?://[^/@\s:]+):[^@\s]+@";

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn redact(text: &str) -> String {
    // Small, allocation-light redaction of https://user:token@host URLs.
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(rel) = find_cred(bytes, i) {
            out.push_str(&text[i..rel]);
            out.push_str("//***@");
            if let Some(at) = text[rel..].find('@') {
                i = rel + at + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    let _ = SECRET_URL;
    out
}

fn find_cred(bytes: &[u8], from: usize) -> Option<usize> {
    let hay = &bytes[from..];
    let mut search = 0;
    while search + 3 < hay.len() {
        if hay[search] == b'/' && hay[search + 1] == b'/' {
            let rest = &hay[search + 2..];
            if let Some(colon) = rest.iter().position(|&b| b == b':') {
                let user = &rest[..colon];
                if !user.is_empty()
                    && !user.contains(&b'/')
                    && !user.contains(&b'@')
                    && !user.contains(&b' ')
                    && rest
                        .get(colon + 1)
                        .is_some_and(|&b| b != b'@' && b != b' ' && b != b'\n')
                    && rest[colon + 1..].contains(&b'@')
                {
                    return Some(from + search);
                }
            }
        }
        search += 1;
    }
    None
}

#[derive(Debug, Clone)]
pub struct CmdResult {
    pub args: Vec<String>,
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdResult {
    pub fn ok(&self) -> bool {
        self.returncode == 0
    }
}

pub fn run_cmd(
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    check: bool,
    quiet: bool,
) -> Result<CmdResult> {
    if args.is_empty() {
        return Err(Error::operational(
            "refusing to run a command string; use an argument array",
        ));
    }
    if verbose() {
        let joined = args.join(" ");
        let extra = cwd
            .map(|c| format!("  (cwd={})", c.display()))
            .unwrap_or_default();
        let _ = writeln!(std::io::stderr(), "$ {joined}{extra}");
    }

    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::dependency(format!(
                "required command not found: {}",
                args[0]
            )));
        }
        Err(e) => return Err(Error::operational(e.to_string())),
    };

    let output = wait_timeout(child, timeout).map_err(|_| {
        Error::operational(format!(
            "command timed out after {}s: {}",
            timeout.as_secs(),
            args[0]
        ))
    })?;

    let result = CmdResult {
        args: args.iter().map(|s| (*s).to_string()).collect(),
        returncode: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };

    if !quiet && verbose() {
        if !result.stdout.trim().is_empty() {
            let t = redact(result.stdout.trim());
            let _ = writeln!(std::io::stderr(), "{}", trunc(&t, 2000));
        }
        if !result.stderr.trim().is_empty() {
            let t = redact(result.stderr.trim());
            let _ = writeln!(std::io::stderr(), "{}", trunc(&t, 2000));
        }
    }

    if check && !result.ok() {
        return Err(Error::operational(format!(
            "command failed ({}): {} — {}",
            result.returncode,
            args[0],
            trunc(&redact(result.stderr.trim()), 400)
        )));
    }
    Ok(result)
}

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

fn wait_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> std::result::Result<std::process::Output, ()> {
    // Drain both pipes while the process runs. Waiting for exit before reading
    // can deadlock as soon as either finite OS pipe buffer fills.
    let stdout = child.stdout.take().ok_or(())?;
    let stderr = child.stderr.take().ok_or(())?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let start = std::time::Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                // Descendants may still hold inherited pipe descriptors. Do
                // not let joining reader threads defeat the timeout.
                return Err(());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    };

    let stdout = stdout_reader.join().map_err(|_| ())??;
    let stderr = stderr_reader.join().map_err(|_| ())??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn read_all(mut pipe: impl Read) -> std::result::Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).map_err(|_| ())?;
    Ok(bytes)
}

pub fn git(cwd: &Path, args: &[&str], timeout: Duration, check: bool) -> Result<CmdResult> {
    let cwd_s = cwd.to_string_lossy();
    // Keep cwd as -C so git never depends on our process cwd.
    // We pass owned strings below.
    let mut owned: Vec<String> = Vec::with_capacity(args.len() + 3);
    owned.push("git".into());
    owned.push("-C".into());
    owned.push(cwd_s.into_owned());
    owned.extend(args.iter().map(|s| (*s).to_string()));
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    run_cmd(&refs, None, timeout, check, false)
}
