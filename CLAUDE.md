# CLAUDE.md

herdr plugin (Rust) that names a herdr pane from the coding agent's
first prompt. When the pane is in an auto-generated linked worktree, it also
renames the worktree branch and workspace. The naming engine is swappable:
on-device Apple FoundationModels by default (a small Swift helper), with a
headless Codex call as the automatic fallback.

## Architecture

Single binary, two phases (`src/main.rs`):

- **Hot phase** (default, every `pane.agent_status_changed` event): pure env-var
  reads, no I/O. `context::evaluate` bails unless the new status is `working` and
  this pane does not already have a done marker. On a pass, writes a pane-scoped
  claim marker and forks the cold phase detached (`setsid`).
- **Cold phase** (`HERDR_NAMING_PHASE=cold`): `herdr::poll_agent_session` →
  `transcript::read_first_prompt` → `main::generate_name` (walks the
  `engine::engine_chain`; fallbacks `slug::display_fallback` for labels and
  `slug::fallback_from_prompt` for branch slugs) → renames configured targets.
  The generated name is also reported as the `task` metadata token on the pane
  and workspace for custom Agent and Space sidebar rows.
  With the default `tab` target on a non-linked workspace, the **title is
  written to `workspace`** (herdr 0.7.4's only bright Agent-sidebar token) and
  the **tab gets the cwd folder basename** (dim). Linked worktrees skip that
  title-on-workspace step so the later branch-slug rename can own the label.
  Multi-tab siblings share one workspace label (last writer wins).
  If the pane is in a linked worktree whose current branch starts with
  `worktree/`, `git::rename_current_branch` renames it to `<prefix>/<slug>` and
  only then `herdr::workspace_rename` renames the workspace to `<slug>`.

Naming outputs: title on workspace (non-worktree); folder on tab; optional
pane/agent renames via `targets`; `$task` metadata always. Branch
`<prefix>/<slug>` (bare `<slug>` when no prefix); workspace `<slug>` after a
successful worktree branch rename. The prefix comes from
`main::resolve_branch_prefix`: `HERDR_NAMING_BRANCH_PREFIX` env, then a
`branch-prefix` file in `HERDR_PLUGIN_CONFIG_DIR`, else none.

`zh` style: CLI engines return a Chinese label + ASCII slug; local fallback
prefers a compact CJK topic (strips spoken fillers) over ASCII kebab when the
prompt has hanzi.

Foundation-generated slugs should be compact noun-topic labels, not literal
sentence summaries. Prefer labels such as `current-file` over
`change-selected-file-to-current`. The helper must ground labels in the actual
prompt and avoid introducing absent concepts from examples or instructions.
The default Foundation path is two-pass: generate several candidates, sanitize
and dedupe them, then ask FoundationModels to select exactly one candidate from
the cleaned list. Codex remains a fallback only when Foundation fails.

## Naming engines

`generate_slug` (in `main.rs`) walks an ordered chain from `engine::engine_chain`,
selected by `HERDR_NAMING_ENGINE`, and uses the first engine that returns a slug:

- unset / `foundation` / unknown → `[Foundation, Codex]` (on-device first)
- `codex` → `[Codex]` only

Each engine returns `Option<String>` and yields `None` on any failure, so the
chain degrades cleanly: Foundation → Codex → deterministic local slug. Engine
binaries are overridable via `HERDR_NAMING_FOUNDATION_BIN` and
`HERDR_NAMING_CODEX_BIN`.

**OS gate:** the `Foundation` engine is `#[cfg(target_os = "macos")]`. Off macOS
(e.g. Linux) the enum variant, the `foundation` module, and the matching
`[[build]]` swift step are all compiled/skipped, so the default chain collapses
to `[Codex]` and a `foundation` request is silently downgraded. The plugin's
`platforms` are `["macos", "linux"]` (Unix only; the cold phase detaches via
`setsid`). Verify the Linux build with
`cargo check --target x86_64-unknown-linux-gnu`.

## Module map

- `context.rs` — parse the two env JSON blobs, working-status eligibility gate
- `slug.rs` — `sanitize` + `fallback_from_prompt`
- `engine.rs` — pure `engine_chain(HERDR_NAMING_ENGINE)` → ordered fallback list
  (OS-aware: Foundation only on macOS)
- `transcript.rs` — resolve transcript path (glob) + first-prompt extraction for
  `claude` and `codex` (different on-disk formats). Claude slash-command
  wrappers are used as a fallback naming prompt, including `command-args`, when
  no normal non-meta user prompt exists; expanded skill bodies remain ignored.
- `foundation.rs` — macOS-only (`#[cfg(target_os = "macos")]`) on-device engine;
  builds a bounded head/tail prompt excerpt, shells to the `herdr-namer` Swift
  helper (15s timeout), sanitizes its stdout
- `codex.rs` — `codex exec --ignore-user-config --ephemeral -s read-only` with a 30s timeout
- `herdr.rs`: `herdr pane get` (polled), pane/workspace task metadata, and
  pane/workspace renames
- `git.rs` — current branch + `git branch -m`
- `naming-helper/` — SwiftPM package (`herdr-namer`): two FoundationModels
  guided-generation calls. The first fills a `@Generable TaskNameCandidates`
  with candidate slugs; the helper sanitizes and dedupes them. The second fills
  a `@Generable SelectedTaskName` by copying one cleaned candidate. The helper
  prints the selected bare slug on stdout (exit 0), or a reason on stderr
  (non-zero) when Apple Intelligence is unavailable or generation fails. Same
  stdout-or-fail contract as `codex`.

## Conventions

- Fail open: every path exits 0; never block herdr.
- First-prompt idempotence is pane-scoped: a fresh claim marker blocks duplicate
  cold phases, and a done marker blocks later events for the same pane.
- The cold phase polls for BOTH the session and the first prompt. Claude reports
  its session at SessionStart (before the prompt) and stays `working` with no new
  event, so a single transcript read can miss the prompt and never retry. Polling
  the prompt (not just the session) is what makes the Claude path reliable.
- Claude slash-command starts count as a prompt fallback. Use the invocation
  wrapper (`command-message`/`command-name` plus `command-args`) for naming, but
  never the expanded skill payload (`isMeta:true`) because it is framework text,
  not user intent.
- Claim marker keyed on pane id in `HERDR_PLUGIN_STATE_DIR`, with a 120s
  staleness TTL; removed on a transient cold-phase miss so a later event retries.
  A separate done marker is written after cold-phase completion.
- Pure logic (context/slug/transcript) is unit-tested; IO edges are
  integration-tested via `herdr plugin link` + `herdr plugin log list`.

## Key facts (verified against herdr 0.7.4, codex-cli 0.142.4, macOS 26.5)

- The generated slug is published as a `task` metadata token on both the pane
  and workspace. Users can render it as `$task` in configurable Agent and Space
  sidebar rows. Metadata reporting is display-only and fail-open; it does not
  gate the persistent rename workflow.

- FoundationModels runs from a plain SwiftPM CLI: no app bundle, Info.plist,
  entitlement, or signing needed to invoke `LanguageModelSession` locally. The
  package floors `platforms` at `.macOS("26.0")` so symbols are reachable
  without per-call `@available`; runtime gating uses
  `SystemLanguageModel.default.availability` (`.deviceNotEligible` /
  `.appleIntelligenceNotEnabled` / `.modelNotReady`), reported as a non-zero
  exit so Rust falls back to Codex.
- The model lives behind a shared OS daemon, so the short-lived helper does not
  reload weights per spawn: warm ~0.3s, cold ~1-2s end-to-end. Both beat the
  Codex bar. Use `greedyOptions(maximumResponseTokens:)` for deterministic
  slugs. That helper selects `GenerationOptions(sampling: .greedy, ...)` on
  Swift 6.2/Xcode 26 and `GenerationOptions(samplingMode: .greedy, ...)` on
  Swift 6.4/Xcode 27, keeping strict warning builds clean while retaining Xcode
  26 support. The source file must not be named `main.swift` (conflicts with
  `@main`).
- Naming uses guided generation, not free text: the model fills `@Generable`
  structs via `respond(to:generating:)` under constrained decoding, so it cannot
  return conversational prose (a plain `respond(to:)` once produced "Sure, here
  are some ideas for..." -> branch `sure-here-are-some-ideas-for`). The helper
  fills named `TaskNameCandidates` slots (`primary`, `artifact`, `outcome`,
  `contextual`, `concise`, `alternate`), then sanitizes and dedupes candidates
  with the same ASCII slug rules as Rust. It then asks a separate judge session
  to fill `SelectedTaskName.slug` by copying exactly one cleaned candidate. If
  the judge invents or mutates a slug, the helper falls back to the first cleaned
  candidate. The candidate guides ask for 1-3 word noun-topic labels, explicitly
  drop generic task verbs, and require concepts from the actual prompt or direct
  synonyms. Avoid unrelated concrete examples in the Foundation prompt: they can
  leak into slugs (for example, an unrelated OAuth example once caused `tell me
  about the commits on this branch` to become `oauth-redirect`).
  `maximumResponseTokens` must clear the JSON envelope plus generated values,
  else a truncated object throws and falls back to Codex; the candidate pass
  currently uses 160 tokens and the judge pass uses 64. The on-device daemon can
  be `.modelNotReady` for the first call(s) after a cold start, so the live
  `cargo test foundation -- --ignored` check is flaky until warm (fails open to
  Codex by design); re-run once warm.

- Foundation prompt input is capped with a head/tail excerpt, not a front-only
  truncation: Rust sends 1200 characters from the start and 1200 from the end
  with an omitted-middle marker when a prompt is long. The Swift helper has the
  same excerpt logic for standalone use. This keeps final user instructions
  visible when long prompts start with pasted context, logs, or prior notes.

- herdr `[[events]]` has NO filter/once/debounce; the hook fires on every event.
- Branch/workspace renaming uses a git safety re-check, not workspace label
  inference: only linked worktrees whose current branch starts with `worktree/`
  are renamed, and workspace rename runs only after branch rename succeeds.
- Panes are renamed with `herdr pane rename`.
- `agent_session` agent label is `claude` or `codex`; transcripts:
  Claude `~/.claude/projects/**/<uuid>.jsonl`, Codex
  `~/.codex/sessions/**/rollout-*<uuid>.jsonl`.
- `--ignore-user-config` on the Codex call disables the user's Codex hooks
  (avoids recursion and nondeterminism); auth still resolves from `CODEX_HOME`.
- Naming model is `gpt-5.5` + `model_reasoning_effort=low` (~2.5s). That is the
  fastest config available on ChatGPT-account auth: `minimal` effort is rejected
  because the `image_gen`/`web_search` tools can't be disabled, and the faster
  `spark`/`flash`/`*-mini` models are API-key-only. `--ephemeral` keeps these
  throwaway runs out of `~/.codex/sessions`.

## Commands

```sh
cargo test                 # unit tests (engine/slug/context/transcript)
cargo test foundation -- --ignored   # live on-device helper check (needs the
                                      # Swift build + Apple Intelligence)
cargo build --release      # produces target/release/herdr-plugin-renamer
cargo fmt                  # format
just build                 # build local Rust and macOS Swift artifacts
just link                  # run build, replace the local herdr link

# On-device naming helper (built by the second [[build]] step on install):
swift build -c release --package-path naming-helper   # -> naming-helper/.build/release/herdr-namer
```

## Fork changes (multi-agent naming, this repo)

This fork diverges from the upstream doc above in these ways:

- Agents: transcript parsers also cover `pi` (JSONL), `grok`
  (`chat_history.jsonl`, prompt inside `<user_query>` tags), and `opencode`
  (`storage/message/<ses>/msg_*.json` + `storage/part/<msg>/prt_*.json`).
  Path-form session values (pi reports the transcript path) are used directly.
  Grok has no integration: `grok.rs` resolves the session from
  `~/.grok/active_sessions.json` by pane foreground pid, else cwd+newest-live.
- Engines: `opencode.rs` (headless `opencode run`, default model
  `opencode/deepseek-v4-flash-free`, knob `opencode-model`) and `claude.rs`
  (plain `claude -p`; extra startup-trimming flags stall on relay setups).
  `engine_chain` accepts comma-separated chains; default macOS chain is
  foundation → opencode → codex → claude. Engine knobs resolve env-first,
  then a same-named file in `HERDR_PLUGIN_CONFIG_DIR`.
- Styles: `style` knob `en`|`zh`. CLI engines receive a style-built two-line
  instruction (`zh`: Chinese label + ASCII slug) and return raw output parsed
  by `slug::parse_engine_output`. Foundation stays ASCII-only.
- Targets: `targets` knob (default `tab`) selects pane/tab/agent renames.
- Idempotence is session-scoped, not pane-scoped: the done marker stores the
  named session id and is checked in the cold phase, so new sessions in a
  long-lived pane are renamed; the hot phase keeps only the claim dedupe.
