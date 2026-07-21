//! Naming engine backed by a headless `opencode run` call. opencode resolves
//! its own configured provider/model, which makes it the engine of choice on
//! machines where Codex and Apple Intelligence are unavailable (e.g. behind
//! relays). Returns `None` on any failure so the caller walks on down the
//! engine chain.

use std::env;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

// Typically 5-10s; occasional slow provider turns need headroom.
const TIMEOUT: Duration = Duration::from_secs(45);

/// Run `opencode run` non-interactively and return its raw stdout for the
/// caller to parse. Runs from the temp dir so opencode does not load project
/// context, and with the herdr pane env stripped so opencode's herdr
/// integration plugin stays inert for this throwaway call.
pub fn generate(instruction: &str) -> Option<String> {
    let bin = resolve_bin()?;

    let mut child = Command::new(bin)
        .arg("run")
        .arg(instruction)
        .current_dir(env::temp_dir())
        .env_remove("HERDR_PANE_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let status = match wait_with_timeout(&mut child, TIMEOUT) {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if !status.success() {
        return None;
    }

    let mut raw = String::new();
    child.stdout.take()?.read_to_string(&mut raw).ok()?;
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

/// Resolve the opencode binary: env override, then the standard install
/// locations (the herdr server's PATH is often minimal under launchd, so a
/// bare name may not resolve), then the bare name as a last resort.
fn resolve_bin() -> Option<String> {
    if let Ok(path) = env::var("HERDR_NAMING_OPENCODE_BIN") {
        if !path.is_empty() {
            return Some(path);
        }
    }
    let home = env::var("HOME").unwrap_or_default();
    for candidate in [
        format!("{home}/.opencode/bin/opencode"),
        "/opt/homebrew/bin/opencode".to_string(),
        "/usr/local/bin/opencode".to_string(),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    Some("opencode".to_string())
}

/// Poll `try_wait` until the child exits or the timeout elapses, returning the
/// exit status if it finished on its own.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}
