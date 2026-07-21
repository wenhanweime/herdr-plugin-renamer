//! Naming engine backed by a headless `claude -p` call. Claude Code resolves
//! the user's configured auth/relay, so this works wherever the user's
//! interactive Claude works. Slower than the other engines (full CLI startup),
//! so it sits at the end of the default chain. Returns `None` on any failure.

use std::env;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

// Claude -p pays full CLI startup (settings, MCP, hooks) plus the model call;
// measured ~45s on a relay setup, so the ceiling is generous.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Run `claude -p --output-format text` and return its raw stdout for the
/// caller to parse. Runs from the temp dir and with the herdr pane env
/// stripped so the user's herdr integration hook stays inert for this
/// throwaway call. No extra startup-trimming flags: several of them stall on
/// relay/router setups, and the plain invocation is the shape verified to work.
pub fn generate(instruction: &str) -> Option<String> {
    let bin = resolve_bin()?;

    let mut command = Command::new(bin);
    command.args(["-p", "--output-format", "text"]);
    if let Ok(model) = env::var("HERDR_NAMING_CLAUDE_MODEL") {
        if !model.is_empty() {
            command.args(["--model", &model]);
        }
    }
    let mut child = command
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

/// Resolve the claude binary: env override, then standard install locations
/// (the herdr server's PATH is often minimal under launchd), then the bare name.
fn resolve_bin() -> Option<String> {
    if let Ok(path) = env::var("HERDR_NAMING_CLAUDE_BIN") {
        if !path.is_empty() {
            return Some(path);
        }
    }
    let home = env::var("HOME").unwrap_or_default();
    for candidate in [
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
        format!("{home}/.claude/local/claude"),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    Some("claude".to_string())
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
