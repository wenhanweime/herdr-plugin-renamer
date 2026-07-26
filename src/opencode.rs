//! Naming engine backed by headless `opencode run` calls. The caller walks the
//! configured free-model list and then falls through to the next engine.

use std::env;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MODELS: &[&str] = &[
    "opencode/deepseek-v4-flash-free",
    "opencode/ling-3.0-flash-free",
    "opencode/mimo-v2.5-free",
];

/// Run `opencode run` non-interactively and return its raw stdout for the
/// caller to parse. Runs from the temp dir so opencode does not load project
/// context, and with the herdr pane env stripped so opencode's herdr
/// integration plugin stays inert for this throwaway call.
pub fn generate(instruction: &str, model: &str) -> Option<String> {
    let bin = resolve_bin()?;

    let mut command = Command::new(bin);
    command.arg("run");
    if model != "default" {
        command.args(["--model", model]);
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
    use super::parse_model_list;

    #[test]
    fn model_list_accepts_commas_and_lines() {
        assert_eq!(
            parse_model_list("a/one, b/two\nc/three\n"),
            vec!["a/one", "b/two", "c/three"]
        );
    }
}
