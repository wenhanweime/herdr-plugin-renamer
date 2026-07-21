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
            "只输出两行，不要任何解释、引号、编号或多余文字。\
             第一行：必须是中文（含汉字），不超过12个字的任务主题名；\
             禁止纯英文、禁止 kebab-case、禁止数字串/指标缩写（如 h1、yoy、1-2-3）。\
             第二行：2-4个英文单词的小写 kebab-case git 分支名（只含字母数字和连字符），\
             用可读的英文词，不要数字段。\
             任务内容：\n\n{truncated}"
        ),
        _ => format!(
            "Output only a short kebab-case git branch slug (2-4 words, lowercase, \
             hyphens only, no prose, no quotes, no surrounding text) summarizing \
             this coding task. Prefer real words over numbers or metric codes \
             (avoid labels like 1-2-3-h1-yoy):\n\n{truncated}"
        ),
    }
}

/// True when `ch` is a CJK Unified Ideograph (common Chinese hanzi range used
/// for zh labels). Keeps the check allocation-free and independent of locales.
fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
    )
}

/// A zh-mode display label must carry at least one hanzi. Pure ASCII / kebab
/// output (common when a free model ignores the two-line format) is rejected
/// so the engine chain can fall through to a prompt-based fallback.
fn is_good_zh_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.chars().any(is_cjk)
}

/// Reject digit-heavy or token-noise slugs that look like garbled metric dumps
/// (`1-2-3-6-h1-yoy`) rather than a readable task name. Pure word slugs pass.
fn is_good_slug(slug: &str) -> bool {
    if slug.is_empty() || slug == "agent-task" {
        return !slug.is_empty();
    }
    let tokens: Vec<&str> = slug.split('-').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return false;
    }
    // A single pure-digit token (`1`) is not a useful tab label.
    if tokens.len() == 1 && tokens[0].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let digit_tokens = tokens
        .iter()
        .filter(|t| t.chars().all(|c| c.is_ascii_digit()))
        .count();
    // Two or more pure-digit segments usually means the model latched onto
    // chart indices / version bits instead of the task topic.
    if digit_tokens >= 2 {
        return false;
    }
    // Mostly non-letter content (digits + short codes) is also noise.
    let alnum: Vec<char> = slug.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if alnum.is_empty() {
        return false;
    }
    let letters = alnum.iter().filter(|c| c.is_ascii_alphabetic()).count();
    if letters * 2 < alnum.len() {
        return false;
    }
    true
}

/// Cap and strip quotes from a candidate display name line.
fn clean_name_line(line: &str) -> String {
    line.trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c.is_whitespace())
        .chars()
        .take(NAME_MAX_CHARS)
        .collect()
}

/// Pick the best Chinese label from engine stdout lines (tail-first, then a
/// reverse scan so status banners do not win over a real name line).
fn pick_zh_name(lines: &[&str]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    // Prefer the conventional "name then slug" layout: second-to-last line.
    if lines.len() >= 2 {
        let candidate = clean_name_line(lines[lines.len() - 2]);
        if is_good_zh_name(&candidate) {
            return Some(candidate);
        }
    }
    // Model sometimes emits only a Chinese line, or buries it above junk.
    for line in lines.iter().rev() {
        let candidate = clean_name_line(line);
        if is_good_zh_name(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Parse a CLI engine's raw output into `(label name, branch slug)` for the
/// given style. `en` keeps the historical behavior (last non-empty line,
/// sanitized, used for both). `zh` expects a name line then a slug line, and
/// degrades gracefully when the model returns only one of them.
///
/// Quality gates: zh labels must contain hanzi; slugs must not be digit-noise.
/// Failures return `None` so the engine chain / local fallback can take over
/// instead of writing garbled tab names like `1-2-3-6-h1-yoy`.
pub fn parse_engine_output(style: &str, raw: &str, prompt: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let last = *lines.last()?;

    if style != "zh" {
        let slug = sanitize(last);
        if slug.is_empty() || !is_good_slug(&slug) {
            return None;
        }
        return Some((slug.clone(), slug));
    }

    let name = pick_zh_name(&lines)?;
    // Prefer the last line as the branch slug when it is clean ASCII; otherwise
    // derive from the prompt so we never ship digit-noise as a branch name.
    let mut slug = sanitize(last);
    if slug.is_empty() || !is_good_slug(&slug) {
        slug = fallback_from_prompt(prompt);
    }
    // If the prompt-derived slug is still empty/noise, keep a stable default.
    if slug.is_empty() || !is_good_slug(&slug) {
        slug = "agent-task".to_string();
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
            Some((
                "优化数据库索引".to_string(),
                "fix-db-index-issue".to_string()
            ))
        );
    }

    #[test]
    fn parse_zh_strips_quotes_and_caps_name() {
        let parsed = parse_engine_output(
            "zh",
            "\"很长的中文任务名称超过十六个字会被截断掉\"\nlong-name",
            "p",
        );
        let (name, slug) = parsed.unwrap();
        assert_eq!(name.chars().count(), 16);
        assert_eq!(slug, "long-name");
    }

    #[test]
    fn parse_zh_rejects_ascii_only_output() {
        // Free models often ignore the zh two-line format and emit only a
        // kebab slug. That must not become the tab label.
        assert!(parse_engine_output("zh", "1-2-3-6-h1-yoy\n", "美团增长调研").is_none());
        assert!(parse_engine_output("zh", "hr-hris-1-ai-2-ai", "招聘AI调研").is_none());
        assert!(
            parse_engine_output("zh", "claude-code-linux-do-v2ex-x\n", "按平台改写宣传帖")
                .is_none()
        );
    }

    #[test]
    fn parse_zh_finds_cjk_name_above_junk_slug() {
        let prompt = "美团平台拉新唤醒用户增长调研";
        let parsed = parse_engine_output(
            "zh",
            "thinking...\n美团拉新增长调研\n1-2-3-6-h1-yoy\n",
            prompt,
        );
        let (name, slug) = parsed.unwrap();
        assert_eq!(name, "美团拉新增长调研");
        // Digit-noise last line is discarded; CJK prompt yields the local default.
        assert_eq!(slug, "agent-task");
        assert!(!slug.contains("1-2-3"));
    }

    #[test]
    fn parse_en_rejects_digit_noise_slugs() {
        assert!(parse_engine_output("", "1-2-3-6-h1-yoy\n", "prompt").is_none());
        assert!(parse_engine_output("", "1\n", "prompt").is_none());
        assert_eq!(
            parse_engine_output("", "fix-db-index\n", "prompt"),
            Some(("fix-db-index".to_string(), "fix-db-index".to_string()))
        );
    }

    #[test]
    fn is_good_slug_accepts_wordy_labels() {
        assert!(is_good_slug("meituan-growth-research"));
        assert!(is_good_slug("hr-ai-efficiency"));
        assert!(!is_good_slug("1-2-3-6-h1-yoy"));
        assert!(!is_good_slug("1"));
    }
}
