//! herdr-plugin-renamer
//!
//! A herdr event hook (`pane.agent_status_changed`) that names a pane (and by
//! default its tab and agent sidebar entry) from the agent's first prompt, and
//! also renames an auto-generated worktree branch/workspace when the pane is
//! in a linked worktree.
//!
//! The binary runs in two phases:
//!   - HOT  (default): fires on every event. Bails in microseconds from env
//!     vars alone unless an agent just started working, then forks the cold
//!     phase detached and exits.
//!   - COLD (`HERDR_NAMING_PHASE=cold`): does the slow work (poll for the
//!     session, read the first prompt, generate a slug via the engine chain,
//!     rename the targets, and maybe rename branch + workspace).
//!
//! Every path exits 0 (fail open) so the hook never wedges herdr.

mod claude;
mod codex;
mod context;
mod engine;
#[cfg(target_os = "macos")]
mod foundation;
mod git;
mod grok;
mod herdr;
mod opencode;
mod slug;
mod transcript;

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A claim marker younger than this is treated as "cold phase in flight" so we
/// don't launch a second Codex call for the same workspace. Older markers are
/// considered stale (e.g. a crashed cold phase) and may be reclaimed.
const CLAIM_TTL: Duration = Duration::from_secs(120);
const SESSION_POLL_ATTEMPTS: u32 = 12;
const SESSION_POLL_DELAY: Duration = Duration::from_millis(500);
const PROMPT_POLL_ATTEMPTS: u32 = 20;
const PROMPT_POLL_DELAY: Duration = Duration::from_millis(750);

fn main() {
    if env::var("HERDR_NAMING_PHASE").as_deref() == Ok("cold") {
        cold_phase();
    } else {
        hot_phase();
    }
}

/// Cheap gate on every event. Reads only env vars; no subprocess, no socket.
fn hot_phase() {
    let event_json = match env::var("HERDR_PLUGIN_EVENT_JSON") {
        Ok(value) => value,
        Err(_) => return,
    };
    let context_json = env::var("HERDR_PLUGIN_CONTEXT_JSON").unwrap_or_default();

    let eligible = match context::evaluate(&event_json, &context_json) {
        Some(eligible) => eligible,
        None => return,
    };
    let marker_key = marker_key_for_pane(&eligible.pane_id);

    let state_dir = state_dir();
    let done_marker = done_marker_path(&state_dir, &marker_key);
    if Path::new(&done_marker).exists() {
        debug_log(&format!(
            "hot: done marker exists, bail pane={} ws={}",
            eligible.pane_id, eligible.workspace_id
        ));
        return;
    }
    let claim_marker = claim_marker_path(&state_dir, &marker_key);
    if claim_is_fresh(&claim_marker) {
        debug_log(&format!(
            "hot: claim fresh, bail pane={} ws={}",
            eligible.pane_id, eligible.workspace_id
        ));
        return;
    }

    let _ = std::fs::create_dir_all(&state_dir);
    let _ = std::fs::write(&claim_marker, now_secs().to_string());

    debug_log(&format!(
        "hot: eligible ws={} pane={} label={:?} linked={} -> fork cold",
        eligible.workspace_id,
        eligible.pane_id,
        eligible.workspace_label,
        eligible.is_linked_worktree
    ));
    spawn_cold_phase(&eligible, &marker_key);
}

/// The slow path, run detached so herdr is never blocked.
fn cold_phase() {
    let pane_id = env::var("HN_PANE_ID").unwrap_or_default();
    let workspace_id = env::var("HN_WORKSPACE_ID").unwrap_or_default();
    let marker_key = env::var("HN_MARKER_KEY").unwrap_or_else(|_| marker_key_for_pane(&pane_id));
    let checkout_path = env::var("HN_CHECKOUT_PATH")
        .ok()
        .filter(|path| !path.is_empty());
    let is_linked_worktree = env::var("HN_IS_LINKED_WORKTREE").as_deref() == Ok("true");
    let state_dir = state_dir();
    let claim_marker = claim_marker_path(&state_dir, &marker_key);
    let done_marker = done_marker_path(&state_dir, &marker_key);
    debug_log(&format!("cold: start ws={workspace_id} pane={pane_id}"));

    // Resolve the native session (with the timing-race poll), then the prompt.
    // On a transient miss, drop the claim so a later event retries.
    let snapshot =
        match herdr::poll_pane_snapshot(&pane_id, SESSION_POLL_ATTEMPTS, SESSION_POLL_DELAY) {
            Some(snapshot) => snapshot,
            None => {
                debug_log("cold: no pane snapshot after poll, removing claim");
                let _ = std::fs::remove_file(&claim_marker);
                return;
            }
        };
    // A reported session (integration hook) wins. Grok has no integration, so
    // its session is recovered from grok's own active-session registry via the
    // pane's foreground pids / cwd.
    let (agent, session_id) = match snapshot.session.clone() {
        Some(session) => session,
        None => match snapshot.detected_agent.as_deref() {
            Some("grok") => match grok::resolve_session(&pane_id, snapshot.cwd.as_deref()) {
                Some(session_id) => ("grok".to_string(), session_id),
                None => {
                    debug_log("cold: grok session not found in active_sessions, removing claim");
                    let _ = std::fs::remove_file(&claim_marker);
                    return;
                }
            },
            other => {
                debug_log(&format!(
                    "cold: no agent_session after poll (detected={other:?}), removing claim"
                ));
                let _ = std::fs::remove_file(&claim_marker);
                return;
            }
        },
    };
    debug_log(&format!("cold: session agent={agent} id={session_id}"));

    // Poll for the first prompt, not just read once. Claude reports its session
    // id at SessionStart (before the prompt is submitted) and flushes the user
    // line a beat after the pane flips to `working`, so a single read can miss
    // it. Since the agent then stays `working` with no new event to retry on, we
    // must wait here rather than bail.
    let prompt = match poll_first_prompt(&agent, &session_id) {
        Some(prompt) => prompt,
        None => {
            debug_log("cold: no first prompt after poll, removing claim");
            let _ = std::fs::remove_file(&claim_marker);
            return;
        }
    };
    debug_log(&format!(
        "cold: first prompt ({} chars): {}",
        prompt.chars().count(),
        prompt.chars().take(80).collect::<String>()
    ));

    // Name it: walk the engine chain (on-device first by default, Codex
    // fallback). If every engine fails, `name` (labels) keeps the prompt's
    // original script via the display fallback while `slug` (git branch) stays
    // ASCII via the deterministic local fallback.
    let slug_file = format!("{state_dir}/{marker_key}.slug");
    let (name, slug) = match generate_slug(&prompt, Path::new(&slug_file)) {
        Some(slug) => (slug.clone(), slug),
        None => {
            let name = slug::display_fallback(&prompt);
            let slug = slug::fallback_from_prompt(&prompt);
            debug_log(&format!(
                "cold: all engines failed, fallback name={name} slug={slug}"
            ));
            (name, slug)
        }
    };

    // Rename the configured targets. Pane is the always-present base; tab and
    // agent are herdr-API extras that default on and can be trimmed via the
    // `targets` knob. Each failure is logged and non-fatal.
    let targets = resolve_targets();
    if targets.iter().any(|t| t == "pane") {
        let ok = herdr::pane_rename(&pane_id, &name);
        debug_log(&format!("cold: pane {pane_id} -> {name} ok={ok}"));
    }
    if targets.iter().any(|t| t == "tab") {
        match snapshot.tab_id.as_deref() {
            Some(tab_id) => {
                let ok = herdr::tab_rename(tab_id, &name);
                debug_log(&format!("cold: tab {tab_id} -> {name} ok={ok}"));
            }
            None => debug_log("cold: skip tab rename, no tab_id in snapshot"),
        }
    }
    if targets.iter().any(|t| t == "agent") {
        // herdr rejects a manual agent name another agent already holds, so
        // retry once with a pane-derived suffix before giving up.
        let mut ok = herdr::agent_rename(&pane_id, &name);
        if !ok {
            let alt = format!("{name}-{}", pane_suffix(&pane_id));
            ok = herdr::agent_rename(&pane_id, &alt);
            debug_log(&format!("cold: agent {pane_id} -> {alt} ok={ok} (retry)"));
        } else {
            debug_log(&format!("cold: agent {pane_id} -> {name} ok={ok}"));
        }
    }

    // Publish the same task name as display metadata so users can place `$task`
    // in custom Agent and Space sidebar rows. Metadata failures do not affect
    // the persistent pane, branch, or workspace renames.
    let pane_metadata_ok = herdr::pane_report_task(&pane_id, &name);
    let workspace_metadata_ok = herdr::workspace_report_task(&workspace_id, &name);
    debug_log(&format!(
        "cold: task metadata pane={pane_metadata_ok} workspace={workspace_metadata_ok}"
    ));

    // Safety re-check: only rename a branch still on the auto `worktree/` name.
    // Only after a successful branch rename do we rename the workspace.
    if is_linked_worktree {
        if let Some(checkout_path) = checkout_path.as_deref() {
            match git::current_branch(checkout_path) {
                Some(current) if current.starts_with("worktree/") => {
                    let branch = compose_branch(resolve_branch_prefix().as_deref(), &slug);
                    let branch_ok = git::rename_current_branch(checkout_path, &branch);
                    debug_log(&format!(
                        "cold: branch {current} -> {branch} ok={branch_ok}"
                    ));
                    if branch_ok {
                        let ok = herdr::workspace_rename(&workspace_id, &slug);
                        debug_log(&format!(
                            "cold: workspace rename ws={workspace_id} -> {slug} ok={ok}"
                        ));
                    } else {
                        debug_log("cold: skip workspace rename, branch rename failed");
                    }
                }
                other => debug_log(&format!(
                    "cold: skip branch/workspace rename, current={other:?}"
                )),
            }
        } else {
            debug_log("cold: skip branch/workspace rename, checkout path unavailable");
        }
    } else {
        debug_log("cold: skip branch/workspace rename, not a linked worktree");
    }

    let _ = std::fs::remove_file(&claim_marker);
    let _ = std::fs::write(&done_marker, now_secs().to_string());
}

/// Walk the engine chain selected by the `HERDR_NAMING_ENGINE` env var (or an
/// `engine` file in the per-plugin config dir), returning the first slug an
/// engine produces. `None` means every engine in the chain failed (so the
/// caller uses the deterministic local fallback).
fn generate_slug(prompt: &str, slug_file: &Path) -> Option<String> {
    let selection = env::var("HERDR_NAMING_ENGINE").ok().or_else(|| {
        let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
        std::fs::read_to_string(format!("{dir}/engine"))
            .ok()
            .map(|s| s.trim().to_string())
    });
    for eng in engine::engine_chain(selection.as_deref()) {
        let result = match eng {
            #[cfg(target_os = "macos")]
            engine::Engine::Foundation => foundation::generate_slug(prompt),
            engine::Engine::Codex => codex::generate_slug(prompt, slug_file),
            engine::Engine::Opencode => opencode::generate_slug(prompt),
            engine::Engine::Claude => claude::generate_slug(prompt),
        };
        match result {
            Some(slug) => {
                debug_log(&format!("cold: {eng:?} slug={slug}"));
                return Some(slug);
            }
            None => debug_log(&format!("cold: {eng:?} produced no slug")),
        }
    }
    None
}

/// Join an optional branch prefix and the slug into the final branch name.
/// Trailing/leading slashes and surrounding whitespace on the prefix are
/// trimmed; an empty or whitespace-only prefix yields the bare slug.
fn compose_branch(prefix: Option<&str>, slug: &str) -> String {
    match prefix
        .map(|p| p.trim().trim_matches('/'))
        .filter(|p| !p.is_empty())
    {
        Some(prefix) => format!("{prefix}/{slug}"),
        None => slug.to_string(),
    }
}

/// Resolve the branch prefix, in priority order: the `HERDR_NAMING_BRANCH_PREFIX`
/// env var (override, incl. set-empty to force no prefix), then a `branch-prefix`
/// file in the per-plugin config dir (`HERDR_PLUGIN_CONFIG_DIR`), else `None` for
/// no prefix. The config file is the install-friendly path: it does not depend on
/// the environment herdr was launched with.
fn resolve_branch_prefix() -> Option<String> {
    if let Ok(prefix) = env::var("HERDR_NAMING_BRANCH_PREFIX") {
        return Some(prefix);
    }
    let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
    std::fs::read_to_string(format!("{dir}/branch-prefix")).ok()
}

/// Resolve which herdr labels get the generated name: the
/// `HERDR_NAMING_TARGETS` env var, then a `targets` file in the per-plugin
/// config dir, else all of them. Comma-separated subset of pane/tab/agent.
fn resolve_targets() -> Vec<String> {
    let raw = env::var("HERDR_NAMING_TARGETS")
        .ok()
        .or_else(|| {
            let dir = env::var("HERDR_PLUGIN_CONFIG_DIR").ok()?;
            std::fs::read_to_string(format!("{dir}/targets")).ok()
        })
        .unwrap_or_default();
    let parsed: Vec<String> = raw
        .split(',')
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if parsed.is_empty() {
        vec!["pane".into(), "tab".into(), "agent".into()]
    } else {
        parsed
    }
}

/// A short unique-ish suffix from the pane id for de-duplicating agent names
/// (e.g. `w4B:p1` -> `p1`).
fn pane_suffix(pane_id: &str) -> String {
    let tail = pane_id.rsplit(':').next().unwrap_or(pane_id);
    let safe: String = tail.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if safe.is_empty() {
        "2".to_string()
    } else {
        safe.to_ascii_lowercase()
    }
}

/// Retry `read_first_prompt` until the transcript has the user's first message
/// or we exhaust the attempts. Covers the lag between the pane reporting
/// `working` and the agent flushing the first user line to its transcript.
fn poll_first_prompt(agent: &str, session_id: &str) -> Option<String> {
    for attempt in 0..PROMPT_POLL_ATTEMPTS {
        if let Some(prompt) = transcript::read_first_prompt(agent, session_id) {
            return Some(prompt);
        }
        if attempt + 1 < PROMPT_POLL_ATTEMPTS {
            std::thread::sleep(PROMPT_POLL_DELAY);
        }
    }
    None
}

/// Re-exec ourselves in the cold phase, detached into a new session so it
/// survives the hot process exiting and any herdr process-group cleanup.
fn spawn_cold_phase(eligible: &context::Eligible, marker_key: &str) {
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return,
    };

    let mut command = Command::new(exe);
    command
        .env("HERDR_NAMING_PHASE", "cold")
        .env("HN_PANE_ID", &eligible.pane_id)
        .env("HN_WORKSPACE_ID", &eligible.workspace_id)
        .env(
            "HN_WORKSPACE_LABEL",
            eligible.workspace_label.as_deref().unwrap_or(""),
        )
        .env(
            "HN_CHECKOUT_PATH",
            eligible.checkout_path.as_deref().unwrap_or(""),
        )
        .env(
            "HN_IS_LINKED_WORKTREE",
            eligible.is_linked_worktree.to_string(),
        )
        .env("HN_MARKER_KEY", marker_key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: setsid only detaches the child into a new session; it does not
    // touch this process's memory and is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let _ = command.spawn();
}

fn state_dir() -> String {
    env::var("HERDR_PLUGIN_STATE_DIR").unwrap_or_else(|_| "/tmp".to_string())
}

fn marker_key_for_pane(pane_id: &str) -> String {
    let safe = pane_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("pane-{safe}")
}

fn claim_marker_path(state_dir: &str, marker_key: &str) -> String {
    format!("{state_dir}/{marker_key}.claim")
}

fn done_marker_path(state_dir: &str, marker_key: &str) -> String {
    format!("{state_dir}/{marker_key}.done")
}

/// True when a claim marker exists and is younger than `CLAIM_TTL`.
fn claim_is_fresh(marker: &str) -> bool {
    let metadata = match std::fs::metadata(marker) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    let modified = match metadata.modified() {
        Ok(modified) => modified,
        Err(_) => return true,
    };
    match modified.elapsed() {
        Ok(age) => age < CLAIM_TTL,
        // Can't read the age: assume fresh and bail rather than double-fire.
        Err(_) => true,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append a diagnostic line to `<state_dir>/debug.log`. Only called on the rare
/// eligible/cold paths, so it never costs the hot-path bail anything. The cold
/// phase runs detached with stderr to /dev/null, so a file is the only way to
/// see what it did.
fn debug_log(message: &str) {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{dir}/debug.log"))
    {
        let _ = writeln!(
            file,
            "{} [pid {}] {}",
            now_secs(),
            std::process::id(),
            message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{compose_branch, marker_key_for_pane, pane_suffix};

    #[test]
    fn pane_suffix_takes_the_pane_segment() {
        assert_eq!(pane_suffix("w4B:p1"), "p1");
        assert_eq!(pane_suffix("solo"), "solo");
        assert_eq!(pane_suffix(":"), "2");
    }

    #[test]
    fn no_prefix_is_bare_slug() {
        assert_eq!(compose_branch(None, "add-dark-mode"), "add-dark-mode");
        assert_eq!(compose_branch(Some(""), "add-dark-mode"), "add-dark-mode");
        assert_eq!(
            compose_branch(Some("   "), "add-dark-mode"),
            "add-dark-mode"
        );
    }

    #[test]
    fn prefix_is_joined_with_a_slash() {
        assert_eq!(
            compose_branch(Some("wyattjoh"), "add-dark-mode"),
            "wyattjoh/add-dark-mode"
        );
    }

    #[test]
    fn surrounding_slashes_and_whitespace_are_trimmed() {
        assert_eq!(
            compose_branch(Some("  /wyattjoh/  "), "add-dark-mode"),
            "wyattjoh/add-dark-mode"
        );
    }

    #[test]
    fn internal_slashes_in_prefix_are_kept() {
        assert_eq!(
            compose_branch(Some("team/wyatt"), "add-dark-mode"),
            "team/wyatt/add-dark-mode"
        );
    }

    #[test]
    fn marker_key_is_safe_for_pane_ids() {
        assert_eq!(marker_key_for_pane("w5V:p1"), "pane-w5V_p1");
    }
}
