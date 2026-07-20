//! Resolving an agent transcript from a native session id and extracting the
//! first genuine user prompt. Supports Claude Code, Codex, Pi, Grok, and
//! opencode, which use different on-disk formats.

use std::env;
use std::path::PathBuf;

/// Resolve the transcript file, read it, and return the first real user prompt.
/// opencode stores one JSON file per message rather than a single transcript
/// file, so it takes a directory-walking path instead of the shared file read.
pub fn read_first_prompt(agent: &str, session_id: &str) -> Option<String> {
    if agent == "opencode" {
        return opencode_first_prompt(session_id);
    }
    let path = resolve_path(agent, session_id)?;
    let contents = std::fs::read_to_string(&path).ok()?;
    first_prompt(agent, &contents)
}

/// Glob the agent's transcript directory for the session's transcript file.
fn resolve_path(agent: &str, session_id: &str) -> Option<PathBuf> {
    let home = env::var("HOME").ok()?;
    let pattern = match agent {
        "claude" => {
            let base = env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| format!("{home}/.claude"));
            format!("{base}/projects/**/{session_id}.jsonl")
        }
        "codex" => {
            let base = env::var("CODEX_HOME").unwrap_or_else(|_| format!("{home}/.codex"));
            format!("{base}/sessions/**/rollout-*{session_id}.jsonl")
        }
        // Pi session files are `<timestamp>_<session_id>.jsonl` under a
        // per-cwd directory. PI_CODING_AGENT_DIR is pi's own dir override.
        "pi" => {
            let base =
                env::var("PI_CODING_AGENT_DIR").unwrap_or_else(|_| format!("{home}/.pi/agent"));
            format!("{base}/sessions/**/*{session_id}.jsonl")
        }
        // Grok sessions are directories named by session id under a per-cwd
        // (URL-encoded) directory; the transcript lives inside.
        "grok" => {
            let base =
                env::var("HERDR_NAMING_GROK_DIR").unwrap_or_else(|_| format!("{home}/.grok"));
            format!("{base}/sessions/**/{session_id}/chat_history.jsonl")
        }
        _ => return None,
    };
    glob::glob(&pattern).ok()?.flatten().next()
}

/// Dispatch to the per-agent transcript parser.
pub fn first_prompt(agent: &str, contents: &str) -> Option<String> {
    match agent {
        "claude" => first_prompt_claude(contents),
        "codex" => first_prompt_codex(contents),
        "pi" => first_prompt_pi(contents),
        "grok" => first_prompt_grok(contents),
        _ => None,
    }
}

/// Context wrappers agents inject as `user` entries ahead of (or instead of)
/// the genuine prompt. Shared by the Pi, Grok, and opencode parsers; Claude
/// and Codex keep their own historical filters.
fn is_wrapped_context(text: &str) -> bool {
    text.starts_with("<user_info>")
        || text.starts_with("<system-reminder>")
        || text.starts_with("<environment_context>")
        || text.starts_with("<user_instructions>")
        || text.starts_with("<INSTRUCTIONS>")
        || text.starts_with("# AGENTS.md")
}

/// Claude Code JSONL: the first `type=="user"` line that is not meta, carries
/// genuine text (string or `text` blocks), and is not a slash/local-command
/// wrapper. If no such line exists, falls back to the first non-ignored
/// slash-command invocation via `claude_command_prompt`.
fn first_prompt_claude(contents: &str) -> Option<String> {
    let mut command_fallback = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if value.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
            continue;
        }
        let content = match value.get("message").and_then(|m| m.get("content")) {
            Some(c) => c,
            None => continue,
        };
        let text = extract_claude_text(content);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if is_claude_command(text) {
            if command_fallback.is_none() {
                command_fallback = claude_command_prompt(text);
            }
            continue;
        }
        return Some(text.to_string());
    }

    command_fallback
}

/// `message.content` is usually a string, sometimes an array of blocks. Only
/// `text` blocks count (tool_result blocks are skipped).
fn extract_claude_text(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

fn is_claude_command(text: &str) -> bool {
    text.starts_with("<command-name")
        || text.starts_with("<command-message")
        || text.starts_with("<local-command")
}

fn claude_command_prompt(text: &str) -> Option<String> {
    if text.starts_with("<local-command") {
        return None;
    }

    // `command-message` is a display label, not the raw slash invocation, but
    // for the builtins in `is_ignored_command` it equals the canonical name
    // (e.g. "clear", "model"). Falls back to `command-name` when absent.
    let command = extract_tag(text, "command-message")
        .or_else(|| extract_tag(text, "command-name"))?
        .trim()
        .trim_start_matches('/')
        .to_string();
    if command.is_empty() {
        return None;
    }
    if is_ignored_command(&command) {
        return None;
    }

    let args = extract_tag(text, "command-args").unwrap_or_default();
    let args = args.trim();

    if args.is_empty() {
        Some(command)
    } else {
        Some(format!("{command} {args}"))
    }
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_string())
}

/// Denylist of Claude Code builtin session-control commands that carry no
/// task intent (settings, housekeeping, meta), so they should never win the
/// command-fallback slot over a later task-bearing command.
fn is_ignored_command(command: &str) -> bool {
    matches!(
        command,
        "add-dir"
            | "bug"
            | "clear"
            | "color"
            | "compact"
            | "config"
            | "cost"
            | "doctor"
            | "export"
            | "help"
            | "login"
            | "logout"
            | "memory"
            | "model"
            | "permissions"
            | "plugin"
            | "plugins"
            | "reload-plugins"
            | "reload-skills"
            | "resume"
            | "skills"
            | "status"
    )
}

/// Codex rollout JSONL: the first `response_item` user `message` whose
/// `input_text` is a real prompt, skipping the developer preamble, the AGENTS.md
/// instruction block, and the `<user_instructions>`/`<environment_context>`
/// wrappers.
fn first_prompt_codex(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        if payload.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let text = payload
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("input_text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let text = text.trim();
        if text.is_empty() || is_codex_preamble(text) {
            continue;
        }
        return Some(text.to_string());
    }
    None
}

fn is_codex_preamble(text: &str) -> bool {
    text.starts_with("# AGENTS.md")
        || text.starts_with("<INSTRUCTIONS>")
        || text.starts_with("<user_instructions>")
        || text.starts_with("<environment_context>")
}

/// Pi JSONL: the first `type=="message"` line whose `message.role=="user"` and
/// whose joined `text` content is a real prompt (not an injected context
/// wrapper). Session-header, model-change, and assistant lines are skipped by
/// the type/role gates.
fn first_prompt_pi(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let message = match value.get("message") {
            Some(m) => m,
            None => continue,
        };
        if message.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let text = join_text_blocks(message.get("content"));
        let text = text.trim();
        if text.is_empty() || is_wrapped_context(text) {
            continue;
        }
        return Some(text.to_string());
    }
    None
}

/// Grok `chat_history.jsonl`: `type=="user"` entries. The genuine request is
/// wrapped in `<user_query>` tags (environment/context entries are separate
/// `user` lines starting with `<user_info>`/`<system-reminder>`). Prefer the
/// first `<user_query>` payload; fall back to the first unwrapped user entry.
fn first_prompt_grok(contents: &str) -> Option<String> {
    let mut plain_fallback = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let text = join_text_blocks(value.get("content"));
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(query) = extract_tag(text, "user_query") {
            let query = query.trim();
            if !query.is_empty() {
                return Some(query.to_string());
            }
            continue;
        }
        if !is_wrapped_context(text) && plain_fallback.is_none() {
            plain_fallback = Some(text.to_string());
        }
    }
    plain_fallback
}

/// Join the `text` fields of `[{type:"text",text:...}]` content blocks. Shared
/// by the Pi and Grok parsers, whose block shape matches.
fn join_text_blocks(content: Option<&serde_json::Value>) -> String {
    content
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// opencode splits a session across `storage/message/<session_id>/msg_*.json`
/// (role metadata) and `storage/part/<message_id>/prt_*.json` (content blocks).
/// Message and part ids are time-ordered, so a filename sort is chronological.
fn opencode_first_prompt(session_id: &str) -> Option<String> {
    let root = opencode_storage_root()?;
    let msg_dir = root.join("message").join(session_id);
    for msg_path in sorted_json_files(&msg_dir) {
        let contents = match std::fs::read_to_string(&msg_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let message_id = match value.get("id").and_then(|i| i.as_str()) {
            Some(id) => id.to_string(),
            None => match msg_path.file_stem().and_then(|s| s.to_str()) {
                Some(stem) => stem.to_string(),
                None => continue,
            },
        };
        let text = opencode_message_text(&root, &message_id);
        let text = text.trim();
        if text.is_empty() || is_wrapped_context(text) {
            continue;
        }
        return Some(text.to_string());
    }
    None
}

/// Concatenate a message's `text` parts in id order.
fn opencode_message_text(root: &std::path::Path, message_id: &str) -> String {
    let part_dir = root.join("part").join(message_id);
    let mut texts = Vec::new();
    for part_path in sorted_json_files(&part_dir) {
        let contents = match std::fs::read_to_string(&part_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }
        if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
            texts.push(text.to_string());
        }
    }
    texts.join("\n")
}

/// opencode data lives under XDG data home (`~/.local/share` by default).
fn opencode_storage_root() -> Option<PathBuf> {
    let data_home = match env::var("XDG_DATA_HOME") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("{}/.local/share", env::var("HOME").ok()?),
    };
    Some(PathBuf::from(data_home).join("opencode/storage"))
}

/// List a directory's `.json` files sorted by filename (ascending).
fn sorted_json_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_string_content() {
        let jsonl = concat!(
            r#"{"type":"summary","summary":"x"}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"Add OAuth login to the dashboard"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("Add OAuth login to the dashboard")
        );
    }

    #[test]
    fn claude_skips_command_wrapper_with_meta_false() {
        let jsonl = concat!(
            r#"{"type":"user","isMeta":false,"message":{"content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"Refactor the parser"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("Refactor the parser")
        );
    }

    #[test]
    fn claude_command_wrapper_falls_back_when_no_prompt() {
        let jsonl = concat!(
            r#"{"type":"user","isMeta":false,"message":{"content":"<command-message>improve-codebase-architecture</command-message>\n<command-name>/improve-codebase-architecture</command-name>"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"Base directory for this skill: /tmp/skills/improve-codebase-architecture\n\n# Improve Codebase Architecture"}]}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("improve-codebase-architecture")
        );
    }

    #[test]
    fn claude_command_wrapper_includes_args() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<command-message>improve-codebase-architecture</command-message>\n<command-name>/improve-codebase-architecture</command-name>\n<command-args>focus on persistence and snapshots</command-args>"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("improve-codebase-architecture focus on persistence and snapshots")
        );
    }

    #[test]
    fn claude_command_fallback_skips_clear_before_task_command() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<command-message>handoff</command-message>\n<command-name>/handoff</command-name>\n<command-args>the next agent should finish the visual pass</command-args>"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("handoff the next agent should finish the visual pass")
        );
    }

    #[test]
    fn claude_command_fallback_skips_session_control_commands() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>claude-opus-4-7</command-args>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<command-message>improve-codebase-architecture</command-message>\n<command-name>/improve-codebase-architecture</command-name>\n<command-args>focus on persistence and snapshots</command-args>"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("improve-codebase-architecture focus on persistence and snapshots")
        );
    }

    #[test]
    fn claude_command_fallback_skips_logout() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<command-name>/logout</command-name>\n<command-message>logout</command-message>\n<command-args></command-args>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<command-message>handoff</command-message>\n<command-name>/handoff</command-name>\n<command-args>the next agent should finish the visual pass</command-args>"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("handoff the next agent should finish the visual pass")
        );
    }

    #[test]
    fn claude_local_command_wrapper_excluded_from_fallback() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<local-command-stdout>ok</local-command-stdout>"}}"#,
            "\n",
        );
        assert!(first_prompt("claude", jsonl).is_none());
    }

    #[test]
    fn claude_command_fallback_uses_command_name_without_message() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"<command-name>/handoff</command-name>\n<command-args>finish the visual pass</command-args>"}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("handoff finish the visual pass")
        );
    }

    #[test]
    fn claude_array_content_skips_tool_result() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"Fix the failing test"}]}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("claude", jsonl).as_deref(),
            Some("Fix the failing test")
        );
    }

    #[test]
    fn codex_skips_preamble_and_instructions() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>"}]}}"#,
            "\n",
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\n<INSTRUCTIONS>"}]}}"##,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd=/x</environment_context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Implement rate limiting on the API"}]}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("codex", jsonl).as_deref(),
            Some("Implement rate limiting on the API")
        );
    }

    #[test]
    fn unknown_agent_returns_none() {
        assert!(first_prompt("gemini", "{}").is_none());
    }

    #[test]
    fn no_real_prompt_returns_none() {
        let jsonl = r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#;
        assert!(first_prompt("claude", jsonl).is_none());
    }

    #[test]
    fn pi_first_user_message() {
        let jsonl = concat!(
            r#"{"type":"session","version":3,"id":"019f","timestamp":"t","cwd":"/x"}"#,
            "\n",
            r#"{"type":"model_change","id":"a","parentId":null,"provider":"Temp","modelId":"m"}"#,
            "\n",
            r#"{"type":"message","id":"b","message":{"role":"user","content":[{"type":"text","text":"调研一下 pi 的插件生态"}]}}"#,
            "\n",
            r#"{"type":"message","id":"c","message":{"role":"assistant","content":[{"type":"text","text":"好的"}]}}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("pi", jsonl).as_deref(),
            Some("调研一下 pi 的插件生态")
        );
    }

    #[test]
    fn pi_skips_wrapped_context_entries() {
        let jsonl = concat!(
            r#"{"type":"message","id":"a","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>skills</system-reminder>"}]}}"#,
            "\n",
            r#"{"type":"message","id":"b","message":{"role":"user","content":[{"type":"text","text":"Fix the login flow"}]}}"#,
            "\n",
        );
        assert_eq!(first_prompt("pi", jsonl).as_deref(), Some("Fix the login flow"));
    }

    #[test]
    fn grok_extracts_user_query_tag() {
        let jsonl = concat!(
            r#"{"type":"system","content":"You are Grok"}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS Version: macos\n</user_info>"}]}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"<system-reminder>context</system-reminder>"}]}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"<user_query>\n帮我优化这个查询\n</user_query>"}]}"#,
            "\n",
        );
        assert_eq!(first_prompt("grok", jsonl).as_deref(), Some("帮我优化这个查询"));
    }

    #[test]
    fn grok_falls_back_to_unwrapped_user_entry() {
        let jsonl = concat!(
            r#"{"type":"user","content":[{"type":"text","text":"<user_info>env</user_info>"}]}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"Plain first prompt"}]}"#,
            "\n",
        );
        assert_eq!(
            first_prompt("grok", jsonl).as_deref(),
            Some("Plain first prompt")
        );
    }

    #[test]
    fn opencode_reads_first_user_message_parts_in_order() {
        let root = std::env::temp_dir().join(format!(
            "renamer-oc-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let msg_dir = root.join("opencode/storage/message/ses_test");
        let part_a = root.join("opencode/storage/part/msg_a");
        let part_b = root.join("opencode/storage/part/msg_b");
        for dir in [&msg_dir, &part_a, &part_b] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // msg_a is assistant, msg_b is the first user message with two parts.
        std::fs::write(
            msg_dir.join("msg_a.json"),
            r#"{"id":"msg_a","sessionID":"ses_test","role":"assistant"}"#,
        )
        .unwrap();
        std::fs::write(
            msg_dir.join("msg_b.json"),
            r#"{"id":"msg_b","sessionID":"ses_test","role":"user"}"#,
        )
        .unwrap();
        std::fs::write(
            part_a.join("prt_1.json"),
            r#"{"id":"prt_1","messageID":"msg_a","type":"text","text":"ignored"}"#,
        )
        .unwrap();
        std::fs::write(
            part_b.join("prt_1.json"),
            r#"{"id":"prt_1","messageID":"msg_b","type":"text","text":"帮我看下"}"#,
        )
        .unwrap();
        std::fs::write(
            part_b.join("prt_2.json"),
            r#"{"id":"prt_2","messageID":"msg_b","type":"file","text":"skip me"}"#,
        )
        .unwrap();

        env::set_var("XDG_DATA_HOME", &root);
        let prompt = read_first_prompt("opencode", "ses_test");
        env::remove_var("XDG_DATA_HOME");
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(prompt.as_deref(), Some("帮我看下"));
    }
}
