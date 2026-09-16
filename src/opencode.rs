//! Naming engine and transcript export backed by headless `opencode` calls.
//! Naming walks the configured free-model list and falls through to the next
//! engine. Transcript export serves the first-prompt reader in transcript.rs.

use std::env;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);
/// A real 745KB export completes in about a second, so the ceiling only
/// bounds a hung export. It is short on purpose: see
/// cold_phase_poll_budget_stays_under_claim_ttl in main.rs, which pins the
/// worst-case poll budget well inside the 120s claim TTL.
pub(crate) const EXPORT_TIMEOUT: Duration = Duration::from_secs(4);
/// Sweeper ceiling for abandoned export temp files. Long enough that a
/// concurrent cold phase's in-flight export is never swept, short enough that
/// leftovers from crashed runs do not accumulate.
const STALE_EXPORT_MAX_AGE: Duration = Duration::from_secs(86_400);
/// Temp-file name prefix shared by the export writer and the sweeper.
const EXPORT_FILE_PREFIX: &str = "herdr-renamer-export-";
const DEFAULT_MODELS: &[&str] = &[
    "opencode/deepseek-v4-flash-free",
    "opencode/ling-3.0-flash-free",
    "opencode/mimo-v2.5-free",
];

/// Build an `opencode` command with the shared hygiene: temp-dir cwd so
/// opencode does not load project context, and the herdr pane env stripped
/// so the herdr integration plugin stays inert.
fn opencode_command(bin: &str) -> Command {
    let mut command = Command::new(bin);
    command
        .current_dir(env::temp_dir())
        .env_remove("HERDR_PANE_ID")
        .stdin(Stdio::null());
    command
}

/// Kill a child that ran past its ceiling and reap it.
fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `opencode run` non-interactively and return its raw stdout for the
/// caller to parse. Spawned through `opencode_command` for the shared
/// subprocess hygiene.
pub fn generate(instruction: &str, model: &str) -> Option<String> {
    let bin = resolve_bin()?;

    let mut command = opencode_command(&bin);
    command.arg("run");
    if model != "default" {
        command.args(["--model", model]);
    }
    let mut child = command
        .arg(instruction)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let status = match wait_with_timeout(&mut child, TIMEOUT) {
        Some(status) => status,
        None => {
            kill_and_reap(&mut child);
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

/// `opencode export <session>` prints the whole session as JSON on stdout.
/// Spawned through `opencode_command` for the shared subprocess hygiene.
/// Stdout goes to a temp file instead of a pipe for two reasons: exports can
/// exceed the OS pipe buffer, and piped export output gets truncated by
/// opencode on large sessions.
pub(crate) fn export_session(session_id: &str) -> Option<String> {
    sweep_stale_exports();
    let bin = resolve_bin()?;
    let temp = env::temp_dir().join(format!(
        "{}{}-{}",
        EXPORT_FILE_PREFIX,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // O_EXCL and owner-only perms: the file holds the user's session transcript.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .ok()?;

    let mut command = opencode_command(&bin);
    command
        .arg("export")
        .arg(session_id)
        .stdout(Stdio::from(file))
        .stderr(Stdio::null());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_file(&temp);
            return None;
        }
    };
    let status = match wait_with_timeout(&mut child, EXPORT_TIMEOUT) {
        Some(status) => status,
        None => {
            kill_and_reap(&mut child);
            let _ = std::fs::remove_file(&temp);
            return None;
        }
    };
    let stdout = std::fs::read_to_string(&temp).unwrap_or_default();
    let _ = std::fs::remove_file(&temp);
    if !status.success() || stdout.trim().is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// Best-effort cleanup of export temp files left behind when a detached cold
/// phase was killed between file creation and removal. Runs once per process
/// (each cold phase is its own process, so repeating the temp-dir scan on
/// every poll attempt would only add I/O). Files older than a day are
/// removed; any error is ignored.
fn sweep_stale_exports() {
    static SWEEP: Once = Once::new();
    SWEEP.call_once(|| {
        let Ok(entries) = std::fs::read_dir(env::temp_dir()) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(EXPORT_FILE_PREFIX) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Some(age) = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
            else {
                continue;
            };
            if age > STALE_EXPORT_MAX_AGE {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}

/// Models resolve from plural env/config, then the legacy singular knob. The
/// legacy value `default` omits `--model` and keeps OpenCode's own selection.
pub fn models() -> Vec<String> {
    let configured = env::var("HERDR_NAMING_OPENCODE_MODELS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
            std::fs::read_to_string(format!("{dir}/opencode-models"))
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| env::var("HERDR_NAMING_OPENCODE_MODEL").ok())
        .or_else(|| {
            let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
            std::fs::read_to_string(format!("{dir}/opencode-model")).ok()
        });
    configured
        .as_deref()
        .map(parse_model_list)
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_MODELS
                .iter()
                .map(|model| model.to_string())
                .collect()
        })
}

fn parse_model_list(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{export_session, parse_model_list};

    #[test]
    fn model_list_accepts_commas_and_lines() {
        assert_eq!(
            parse_model_list("a/one, b/two\nc/three\n"),
            vec!["a/one", "b/two", "c/three"]
        );
    }

    /// Live fail-open contract for the export subprocess. Run with:
    /// cargo test export -- --ignored
    /// Needs the opencode CLI installed; without it the spawn fails and the
    /// None contract holds trivially.
    #[test]
    #[ignore]
    fn export_session_fails_open_on_missing_session() {
        assert!(export_session("ses_does_not_exist_0000000000").is_none());
    }
}
