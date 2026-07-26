//! Naming engine backed by non-interactive Pi calls with model fallback.

use std::env;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MODEL: &str = "default";

/// Run one ephemeral, tool-free Pi naming attempt with an explicit model.
pub fn generate(instruction: &str, model: &str) -> Option<String> {
    let bin = resolve_bin()?;
    let mut command = Command::new(bin);
    command.args([
        "--print",
        "--mode",
        "text",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--thinking",
        "off",
    ]);
    if model != DEFAULT_MODEL {
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
    (!raw.trim().is_empty()).then_some(raw)
}

/// Models resolve from env, then `pi-models`, then Pi's configured default.
/// The literal value `default` omits `--model`, allowing Pi to select it.
pub fn models() -> Vec<String> {
    let configured = env::var("HERDR_NAMING_PI_MODELS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| read_config("pi-models"));
    configured
        .as_deref()
        .map(parse_model_list)
        .filter(|models| !models.is_empty())
        .unwrap_or_else(|| vec![DEFAULT_MODEL.to_string()])
}

fn parse_model_list(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_config(knob: &str) -> Option<String> {
    let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
    std::fs::read_to_string(format!("{dir}/{knob}"))
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn resolve_bin() -> Option<String> {
    if let Ok(path) = env::var("HERDR_NAMING_PI_BIN") {
        if !path.is_empty() {
            return Some(path);
        }
    }
    let home = env::var("HOME").unwrap_or_default();
    for candidate in [
        format!("{home}/.bun/bin/pi"),
        "/opt/homebrew/bin/pi".to_string(),
        "/usr/local/bin/pi".to_string(),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    Some("pi".to_string())
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if start.elapsed() >= timeout => return None,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{models, parse_model_list};

    #[test]
    fn model_list_accepts_commas_and_lines() {
        assert_eq!(
            parse_model_list("a/one, b/two\nc/three\n"),
            vec!["a/one", "b/two", "c/three"]
        );
    }

    #[test]
    fn unconfigured_models_use_pi_default() {
        if std::env::var_os("HERDR_NAMING_PI_MODELS").is_none()
            && std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").is_none()
        {
            assert_eq!(models(), vec!["default"]);
        }
    }
}
