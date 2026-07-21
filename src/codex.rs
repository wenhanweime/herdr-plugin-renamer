//! The naming engine: a single headless Codex call that turns the captured
//! prompt into a kebab-case slug. Bounded by a hard timeout; returns `None` on
//! any failure so the caller can fall back to a deterministic local slug.

use std::env;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Run `codex exec` non-interactively and return the raw final message for the
/// caller to parse. The model's final message is written to `out_file` via
/// `-o`; we read it back.
///
/// `--ignore-user-config` is load-bearing: it disables the user's Codex hooks
/// (SessionStart/UserPromptSubmit, including herdr's own), giving a
/// deterministic, recursion-free run. Auth still resolves from CODEX_HOME.
pub fn generate(instruction: &str, out_file: &Path) -> Option<String> {
    let bin = env::var("HERDR_NAMING_CODEX_BIN").unwrap_or_else(|_| "codex".to_string());

    let _ = std::fs::remove_file(out_file);

    let mut child = Command::new(bin)
        .args([
            "exec",
            "--skip-git-repo-check",
            "--ignore-user-config",
            // Don't persist a rollout session file for these throwaway naming runs.
            "--ephemeral",
            "-s",
            "read-only",
            // gpt-5.5 + low is the fastest config available on ChatGPT-account auth:
            // minimal effort is rejected (image_gen/web_search can't be disabled) and
            // the faster spark/flash/mini models require API-key auth.
            "-m",
            "gpt-5.5",
            "-c",
            "model_reasoning_effort=low",
            // Request the fast/priority service tier to shave queue latency.
            "-c",
            "service_tier=fast",
            "-o",
        ])
        .arg(out_file)
        .arg(instruction)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if !wait_with_timeout(&mut child, TIMEOUT) {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let raw = std::fs::read_to_string(out_file).ok()?;
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

/// Poll `try_wait` until the child exits or the timeout elapses. Returns true if
/// the child finished on its own.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}
