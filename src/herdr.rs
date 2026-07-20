//! Calls back into herdr over its CLI: reading a pane's native agent session
//! (with a short poll for the documented timing race) and renaming Herdr labels.

use std::env;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

const METADATA_SOURCE: &str = "plugin:herdr-plugin-renamer";

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

/// What the cold phase needs from one `pane get` call: the reported native
/// session (absent until the integration hook lands), the detected agent label
/// (present even for agents with no integration, e.g. grok), and the pane's
/// tab/cwd for the extra rename targets.
#[derive(Debug, Default, Clone)]
pub struct PaneSnapshot {
    pub session: Option<(String, String)>,
    pub detected_agent: Option<String>,
    pub tab_id: Option<String>,
    pub cwd: Option<String>,
}

/// `herdr pane get <pane_id>` returns JSON by default.
fn pane_snapshot(pane_id: &str) -> Option<PaneSnapshot> {
    let output = Command::new(herdr_bin())
        .args(["pane", "get", pane_id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    // The CLI wraps the pane in a `{"result":{"pane":{...}}}` envelope. Accept
    // the wrapped shape first, then fall back to unwrapped variants.
    let pane = value
        .pointer("/result/pane")
        .or_else(|| value.get("pane"))
        .unwrap_or(&value);

    let session = pane.get("agent_session").and_then(|session| {
        let agent = match session.get("agent").and_then(|a| a.as_str()) {
            Some(a) => a.to_string(),
            // Older builds emit only `source` (e.g. "herdr:claude").
            None => session
                .get("source")
                .and_then(|s| s.as_str())
                .map(|s| s.trim_start_matches("herdr:").to_string())?,
        };
        let value = session.get("value").and_then(|v| v.as_str())?.to_string();
        Some((agent, value))
    });

    Some(PaneSnapshot {
        session,
        detected_agent: pane
            .get("agent")
            .and_then(|a| a.as_str())
            .map(|a| a.to_string()),
        tab_id: pane
            .get("tab_id")
            .and_then(|t| t.as_str())
            .map(|t| t.to_string()),
        cwd: pane
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(|c| c.to_string()),
    })
}

/// Poll `pane get` for the session id. `pane.agent_status_changed` can fire
/// before herdr has received the session from the integration hook, so we retry
/// briefly before giving up. Agents with no integration never report a session:
/// once the detected label identifies one of those (grok), return immediately
/// and let the caller resolve the session its own way. On exhaustion the last
/// snapshot is returned so the caller can still try a label-based fallback.
pub fn poll_pane_snapshot(pane_id: &str, attempts: u32, delay: Duration) -> Option<PaneSnapshot> {
    let mut last = None;
    for attempt in 0..attempts {
        if let Some(snapshot) = pane_snapshot(pane_id) {
            if snapshot.session.is_some() {
                return Some(snapshot);
            }
            if snapshot.detected_agent.as_deref() == Some("grok") {
                return Some(snapshot);
            }
            last = Some(snapshot);
        }
        if attempt + 1 < attempts {
            sleep(delay);
        }
    }
    last
}

/// `herdr pane process-info` — the pane's foreground process ids (plus the
/// foreground process group id, which for a TUI agent is its own pid).
pub fn pane_foreground_pids(pane_id: &str) -> Vec<u32> {
    let output = match Command::new(herdr_bin())
        .args(["pane", "process-info", "--pane", pane_id])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let value: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let info = value
        .pointer("/result/process_info")
        .or_else(|| value.get("process_info"))
        .unwrap_or(&value);

    let mut pids = Vec::new();
    if let Some(processes) = info.get("foreground_processes").and_then(|p| p.as_array()) {
        for process in processes {
            if let Some(pid) = process.get("pid").and_then(|p| p.as_u64()) {
                pids.push(pid as u32);
            }
        }
    }
    if let Some(pgid) = info
        .get("foreground_process_group_id")
        .and_then(|p| p.as_u64())
    {
        let pgid = pgid as u32;
        if !pids.contains(&pgid) {
            pids.push(pgid);
        }
    }
    pids
}

/// `herdr workspace rename <workspace_id> <label>`.
pub fn workspace_rename(workspace_id: &str, label: &str) -> bool {
    Command::new(herdr_bin())
        .args(["workspace", "rename", workspace_id, label])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `herdr pane rename <pane_id> <label>`.
pub fn pane_rename(pane_id: &str, label: &str) -> bool {
    Command::new(herdr_bin())
        .args(["pane", "rename", pane_id, label])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `herdr tab rename <tab_id> <label>`.
pub fn tab_rename(tab_id: &str, label: &str) -> bool {
    Command::new(herdr_bin())
        .args(["tab", "rename", tab_id, label])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `herdr agent rename <target> <name>` — targets accept pane ids. Fails when
/// another agent already holds the manual name (herdr enforces uniqueness).
pub fn agent_rename(target: &str, name: &str) -> bool {
    Command::new(herdr_bin())
        .args(["agent", "rename", target, name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Publish the generated task name for custom Agent sidebar rows via `$task`.
pub fn pane_report_task(pane_id: &str, task: &str) -> bool {
    report_task_metadata("pane", pane_id, task)
}

/// Publish the generated task name for custom Space sidebar rows via `$task`.
pub fn workspace_report_task(workspace_id: &str, task: &str) -> bool {
    report_task_metadata("workspace", workspace_id, task)
}

fn report_task_metadata(resource: &str, resource_id: &str, task: &str) -> bool {
    let token = format!("task={task}");
    Command::new(herdr_bin())
        .args([
            resource,
            "report-metadata",
            resource_id,
            "--source",
            METADATA_SOURCE,
            "--token",
            &token,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
