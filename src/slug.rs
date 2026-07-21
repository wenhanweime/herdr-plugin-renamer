//! Turning arbitrary text into a safe kebab-case slug, and a deterministic
//! fallback slug derived from the prompt when the Codex naming engine is
//! unavailable.

const MAX_WORDS: usize = 6;
const MAX_LEN: usize = 50;

/// Lowercase, collapse every run of non-alphanumeric characters into a single
/// hyphen, trim leading/trailing hyphens, then cap to `MAX_WORDS` words and
/// `MAX_LEN` characters. ASCII-only output suitable for a git branch name.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true; // start true so leading separators are dropped
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }

    let capped = out
        .split('-')
        .filter(|w| !w.is_empty())
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join("-");

    let mut capped = if capped.len() > MAX_LEN {
        capped[..MAX_LEN].to_string()
    } else {
        capped
    };
    while capped.ends_with('-') {
        capped.pop();
    }
    capped
}

/// Build a slug from the first non-empty line of the prompt. Never returns an
/// empty string, so a rename always has something to use.
pub fn fallback_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let slug = sanitize(first_line);
    if slug.is_empty() {
        "agent-task".to_string()
    } else {
        slug
    }
}

const DISPLAY_MAX_CHARS: usize = 24;

/// A display name for pane/tab/agent labels when every naming engine failed.
/// `sanitize` is ASCII-only, so a CJK prompt would collapse to the generic
/// `agent-task`; labels (unlike git branches) can carry the original script,
/// so fall back to a capped excerpt of the prompt's first line instead.
pub fn display_fallback(prompt: &str) -> String {
    let ascii = fallback_from_prompt(prompt);
    if ascii != "agent-task" {
        return ascii;
    }
    let first_line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let compact = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = compact.chars().take(DISPLAY_MAX_CHARS).collect();
    if capped.is_empty() {
        "agent-task".to_string()
    } else {
        capped
    }
}

const INSTRUCTION_HEAD_CHARS: usize = 500;
const INSTRUCTION_TAIL_CHARS: usize = 300;
const NAME_MAX_CHARS: usize = 16;

/// A head+tail excerpt of the prompt for the engine instruction. Long prompts
/// are usually pasted context with the actual request at one end; a short
/// excerpt names just as well and keeps small relay models fast.
fn instruction_excerpt(prompt: &str) -> String {
    let char_count = prompt.chars().count();
    if char_count <= INSTRUCTION_HEAD_CHARS + INSTRUCTION_TAIL_CHARS {
        return prompt.to_string();
    }
    let head: String = prompt.chars().take(INSTRUCTION_HEAD_CHARS).collect();
    let tail_start = char_count.saturating_sub(INSTRUCTION_TAIL_CHARS);
    let tail: String = prompt.chars().skip(tail_start).collect();
    format!("{head}\n\n[...中间省略...]\n\n{tail}")
}

/// Build the instruction handed to a CLI naming engine. `en` asks for the
/// historical single kebab slug; `zh` asks for a Chinese label line plus an
/// ASCII branch-slug line so labels can carry CJK while branches stay ASCII.
pub fn engine_instruction(style: &str, prompt: &str) -> String {
    let truncated = instruction_excerpt(prompt);
    match style {
        "zh" => format!(
            "只输出两行，不要任何解释、引号或多余文字。\
             第一行：不超过12个字的中文任务名，概括下面这个编码任务；\
             第二行：2-4个英文单词的小写 kebab-case git 分支名（只含字母数字和连字符）。\
             任务内容：\n\n{truncated}"
        ),
        _ => format!(
            "Output only a short kebab-case git branch slug (2-4 words, lowercase, \
             hyphens only, no prose, no quotes, no surrounding text) summarizing \
             this coding task:\n\n{truncated}"
        ),
    }
}

/// Parse a CLI engine's raw output into `(label name, branch slug)` for the
/// given style. `en` keeps the historical behavior (last non-empty line,
/// sanitized, used for both). `zh` expects a name line then a slug line, and
/// degrades gracefully when the model returns only one of them.
pub fn parse_engine_output(style: &str, raw: &str, prompt: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let last = *lines.last()?;

    if style != "zh" {
        let slug = sanitize(last);
        if slug.is_empty() {
            return None;
        }
        return Some((slug.clone(), slug));
    }

    // Engines may print status lines first, so read from the tail: the slug is
    // the last line, the Chinese name the one before it (or the same line when
    // the model collapsed to a single line).
    let name_line = if lines.len() >= 2 {
        lines[lines.len() - 2]
    } else {
        last
    };
    let name: String = name_line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c.is_whitespace())
        .chars()
        .take(NAME_MAX_CHARS)
        .collect();
    if name.is_empty() {
        return None;
    }
    let mut slug = sanitize(last);
    if slug.is_empty() {
        slug = fallback_from_prompt(prompt);
    }
    Some((name, slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_kebab() {
        assert_eq!(sanitize("OAuth Login Providers"), "oauth-login-providers");
    }

    #[test]
    fn collapses_punctuation_and_spaces() {
        assert_eq!(
            sanitize("Fix the bug!!! in   parser"),
            "fix-the-bug-in-parser"
        );
    }

    #[test]
    fn trims_edges() {
        assert_eq!(sanitize("  --Hello, World--  "), "hello-world");
    }

    #[test]
    fn caps_to_six_words() {
        assert_eq!(
            sanitize("one two three four five six seven eight"),
            "one-two-three-four-five-six"
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(sanitize("   !!!   "), "");
    }

    #[test]
    fn fallback_never_empty() {
        assert_eq!(fallback_from_prompt("!!!"), "agent-task");
        assert_eq!(
            fallback_from_prompt("Add JWT auth to the API endpoints please"),
            "add-jwt-auth-to-the-api"
        );
    }

    #[test]
    fn fallback_uses_first_nonempty_line() {
        assert_eq!(
            fallback_from_prompt("\n\n  \nRefactor token validation"),
            "refactor-token-validation"
        );
    }

    #[test]
    fn display_fallback_prefers_ascii_slug() {
        assert_eq!(
            display_fallback("Add JWT auth to the API endpoints please"),
            "add-jwt-auth-to-the-api"
        );
    }

    #[test]
    fn display_fallback_keeps_cjk_prompts() {
        assert_eq!(display_fallback("帮我优化数据库查询"), "帮我优化数据库查询");
    }

    #[test]
    fn display_fallback_caps_long_cjk_prompts() {
        let long = "这是一个非常长的中文提示词需要被截断".repeat(3);
        assert_eq!(display_fallback(&long).chars().count(), 24);
    }

    #[test]
    fn display_fallback_never_empty() {
        assert_eq!(display_fallback("!!!"), "!!!");
        assert_eq!(display_fallback(""), "agent-task");
    }

    #[test]
    fn parse_en_takes_last_line_for_both() {
        let parsed = parse_engine_output("", "thinking...\nfix-db-index\n", "prompt");
        assert_eq!(
            parsed,
            Some(("fix-db-index".to_string(), "fix-db-index".to_string()))
        );
        assert!(parse_engine_output("", "！！！\n", "prompt").is_none());
    }

    #[test]
    fn parse_zh_takes_name_then_slug() {
        let parsed = parse_engine_output("zh", "优化数据库索引\nfix-db-index\n", "prompt");
        assert_eq!(
            parsed,
            Some(("优化数据库索引".to_string(), "fix-db-index".to_string()))
        );
    }

    #[test]
    fn parse_zh_skips_leading_status_lines() {
        let parsed = parse_engine_output("zh", "banner line\n优化数据库索引\nfix-db-index", "p");
        assert_eq!(
            parsed,
            Some(("优化数据库索引".to_string(), "fix-db-index".to_string()))
        );
    }

    #[test]
    fn parse_zh_single_line_derives_slug_from_prompt() {
        let parsed = parse_engine_output("zh", "优化数据库索引", "Fix db index issue");
        assert_eq!(
            parsed,
            Some(("优化数据库索引".to_string(), "fix-db-index-issue".to_string()))
        );
    }

    #[test]
    fn parse_zh_strips_quotes_and_caps_name() {
        let parsed = parse_engine_output("zh", "\"很长的中文任务名称超过十六个字会被截断掉\"\nlong-name", "p");
        let (name, slug) = parsed.unwrap();
        assert_eq!(name.chars().count(), 16);
        assert_eq!(slug, "long-name");
    }
}
