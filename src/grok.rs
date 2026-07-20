//! Resolving a Grok session without an integration hook. Grok has no herdr
//! integration, so panes running it never get an `agent_session` report; herdr
//! still detects the agent (label `grok`) and tracks its status. Grok itself
//! maintains `~/.grok/active_sessions.json` mapping every live session to its
//! process id and cwd, which is enough to recover the session id:
//!
//!   1. match a pane foreground pid against a session pid (exact), else
//!   2. match the pane cwd and take the most recently opened live session.

use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    pub session_id: String,
    pub pid: u32,
    pub cwd: String,
    pub opened_at: String,
}

/// Resolve the session id for a grok pane, or `None` when grok's session file
/// is missing or nothing matches the pane's pids/cwd.
pub fn resolve_session(pane_id: &str, pane_cwd: Option<&str>) -> Option<String> {
    let sessions = read_active_sessions()?;
    let pids = crate::herdr::pane_foreground_pids(pane_id);
    pick_session(&sessions, &pids, pane_cwd, pid_is_alive)
}

/// The pure matching core, parameterized on liveness so tests don't need real
/// processes. Pid match wins outright (no liveness check needed: the pid came
/// from the live pane). The cwd fallback filters to live sessions because the
/// file can retain entries from crashed runs.
fn pick_session(
    sessions: &[ActiveSession],
    pane_pids: &[u32],
    pane_cwd: Option<&str>,
    alive: fn(u32) -> bool,
) -> Option<String> {
    if let Some(session) = sessions.iter().find(|s| pane_pids.contains(&s.pid)) {
        return Some(session.session_id.clone());
    }
    let cwd = pane_cwd?;
    sessions
        .iter()
        .filter(|s| s.cwd == cwd && alive(s.pid))
        // opened_at is RFC 3339, so the lexicographic max is the newest.
        .max_by(|a, b| a.opened_at.cmp(&b.opened_at))
        .map(|s| s.session_id.clone())
}

fn read_active_sessions() -> Option<Vec<ActiveSession>> {
    let base = env::var("HERDR_NAMING_GROK_DIR")
        .unwrap_or_else(|_| format!("{}/.grok", env::var("HOME").unwrap_or_default()));
    let contents = std::fs::read_to_string(format!("{base}/active_sessions.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let entries = value.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                Some(ActiveSession {
                    session_id: entry.get("session_id")?.as_str()?.to_string(),
                    pid: entry.get("pid")?.as_u64()? as u32,
                    cwd: entry.get("cwd")?.as_str()?.to_string(),
                    opened_at: entry
                        .get("opened_at")
                        .and_then(|o| o.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect(),
    )
}

/// Signal 0 probes for existence without delivering anything. EPERM would also
/// mean "alive", but every grok process here belongs to the same user.
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only performs the permission/existence check.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, pid: u32, cwd: &str, opened_at: &str) -> ActiveSession {
        ActiveSession {
            session_id: id.to_string(),
            pid,
            cwd: cwd.to_string(),
            opened_at: opened_at.to_string(),
        }
    }

    fn always_alive(_pid: u32) -> bool {
        true
    }

    fn never_alive(_pid: u32) -> bool {
        false
    }

    #[test]
    fn exact_pid_match_wins() {
        let sessions = vec![
            session("s-old", 100, "/x", "2026-07-19T00:00:00Z"),
            session("s-mine", 200, "/x", "2026-07-18T00:00:00Z"),
        ];
        assert_eq!(
            pick_session(&sessions, &[200], Some("/x"), always_alive).as_deref(),
            Some("s-mine")
        );
    }

    #[test]
    fn cwd_fallback_takes_newest_live_session() {
        let sessions = vec![
            session("s-a", 100, "/x", "2026-07-19T00:00:00Z"),
            session("s-b", 101, "/x", "2026-07-20T00:00:00Z"),
            session("s-c", 102, "/y", "2026-07-21T00:00:00Z"),
        ];
        assert_eq!(
            pick_session(&sessions, &[999], Some("/x"), always_alive).as_deref(),
            Some("s-b")
        );
    }

    #[test]
    fn dead_sessions_are_ignored_in_cwd_fallback() {
        let sessions = vec![session("s-a", 100, "/x", "2026-07-19T00:00:00Z")];
        assert!(pick_session(&sessions, &[999], Some("/x"), never_alive).is_none());
    }

    #[test]
    fn no_cwd_and_no_pid_match_is_none() {
        let sessions = vec![session("s-a", 100, "/x", "t")];
        assert!(pick_session(&sessions, &[999], None, always_alive).is_none());
    }
}
