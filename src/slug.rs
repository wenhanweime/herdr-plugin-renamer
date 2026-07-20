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
}
