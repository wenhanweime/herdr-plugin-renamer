//! Naming-engine selection: maps the `HERDR_NAMING_ENGINE` knob to an ordered
//! fallback chain. The cold phase tries each engine in turn and uses the first
//! that returns a slug, falling back to a deterministic local slug if all fail.
//!
//! The knob accepts a single engine name or a comma-separated chain
//! (`pi,opencode`). Unset, empty, or unknown values resolve to the default
//! Pi-then-OpenCode chain.
//!
//! The on-device `Foundation` engine is macOS-only and is compiled out entirely
//! on other targets (e.g. Linux): the enum variant does not exist there, so a
//! non-macOS build can neither select nor reference Apple's FoundationModels.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// On-device Apple FoundationModels via the `herdr-namer` Swift helper.
    #[cfg(target_os = "macos")]
    Foundation,
    /// Headless `codex exec` call.
    Codex,
    /// Headless `opencode run` call (uses the user's configured provider).
    Opencode,
    /// Headless `claude -p` call (uses the user's configured auth/relay).
    Claude,
    /// Headless Pi calls with a configurable model fallback list.
    Pi,
}

/// Resolve the engine knob to the ordered list of engines to try.
///
/// - a single name (`pi`, `opencode`, `codex`, `claude`): that engine only.
/// - a comma-separated list: those engines, in order (unknown names dropped).
/// - unset, empty, or unknown: Pi followed by OpenCode.
/// - `foundation`: the on-device macOS engine only; off macOS it resolves to
///   the default chain because that engine is unavailable.
pub fn engine_chain(selection: Option<&str>) -> Vec<Engine> {
    let normalized = selection
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if normalized.is_empty() {
        return default_chain();
    }
    let parsed: Vec<Engine> = normalized
        .split(',')
        .filter_map(|name| match name.trim() {
            #[cfg(target_os = "macos")]
            "foundation" => Some(Engine::Foundation),
            "codex" => Some(Engine::Codex),
            "opencode" => Some(Engine::Opencode),
            "claude" => Some(Engine::Claude),
            "pi" => Some(Engine::Pi),
            _ => None,
        })
        .collect();
    if parsed.is_empty() {
        default_chain()
    } else {
        parsed
    }
}

#[cfg(target_os = "macos")]
fn default_chain() -> Vec<Engine> {
    vec![Engine::Pi, Engine::Opencode]
}

#[cfg(not(target_os = "macos"))]
fn default_chain() -> Vec<Engine> {
    vec![Engine::Pi, Engine::Opencode]
}

#[cfg(test)]
mod tests {
    use super::*;

    // `codex` is always honored and always Codex-only, on every platform.
    #[test]
    fn codex_selection_skips_foundation() {
        assert_eq!(engine_chain(Some("codex")), vec![Engine::Codex]);
    }

    #[test]
    fn selection_is_case_and_whitespace_insensitive() {
        assert_eq!(engine_chain(Some("  CODEX ")), vec![Engine::Codex]);
    }

    #[test]
    fn single_cli_engines_are_honored() {
        assert_eq!(engine_chain(Some("pi")), vec![Engine::Pi]);
        assert_eq!(engine_chain(Some("opencode")), vec![Engine::Opencode]);
        assert_eq!(engine_chain(Some("claude")), vec![Engine::Claude]);
    }

    #[test]
    fn comma_chain_preserves_order_and_drops_unknowns() {
        assert_eq!(
            engine_chain(Some("opencode, claude")),
            vec![Engine::Opencode, Engine::Claude]
        );
        assert_eq!(
            engine_chain(Some("bogus,claude,codex")),
            vec![Engine::Claude, Engine::Codex]
        );
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        #[test]
        fn default_chain_is_pi_then_opencode() {
            assert_eq!(engine_chain(None), vec![Engine::Pi, Engine::Opencode]);
        }

        #[test]
        fn foundation_selection_is_literal() {
            assert_eq!(engine_chain(Some(" Foundation ")), vec![Engine::Foundation]);
        }

        #[test]
        fn unknown_or_empty_falls_back_to_default_chain() {
            assert_eq!(engine_chain(Some("bogus")), engine_chain(None));
            assert_eq!(engine_chain(Some("")), engine_chain(None));
        }

        #[test]
        fn foundation_in_a_comma_chain_is_literal() {
            assert_eq!(
                engine_chain(Some("foundation,opencode")),
                vec![Engine::Foundation, Engine::Opencode]
            );
        }
    }

    // Off macOS there is no on-device engine: `foundation` resolves to the
    // default CLI chain rather than being attempted.
    #[cfg(not(target_os = "macos"))]
    mod non_macos {
        use super::*;

        #[test]
        fn default_chain_is_pi_then_opencode() {
            assert_eq!(engine_chain(None), vec![Engine::Pi, Engine::Opencode]);
        }

        #[test]
        fn foundation_request_is_downgraded_to_the_default_chain() {
            assert_eq!(engine_chain(Some("foundation")), engine_chain(None));
        }

        #[test]
        fn unknown_or_empty_is_the_default_chain() {
            assert_eq!(engine_chain(Some("bogus")), engine_chain(None));
            assert_eq!(engine_chain(Some("")), engine_chain(None));
        }
    }
}
