# herdr-plugin-renamer

A [herdr](https://herdr.dev) plugin that names panes from a coding agent's first
prompt. In auto-generated linked worktrees, it also renames the git branch and
workspace.

It supports Claude Code and Codex. Slugs come from Apple FoundationModels on
supported Macs, then Codex, then a deterministic local fallback.

## Install

```sh
herdr plugin install wyattjoh/herdr-plugin-renamer
```

Install the herdr integration for each agent you use:

```sh
herdr integration install claude
herdr integration install codex
```

## Requirements

- herdr 0.7.4+ on macOS or Linux
- For on-device naming: macOS 26+ on Apple Silicon with Apple Intelligence
  enabled
- For Codex naming: the `codex` CLI on `PATH` and logged in

Without either naming model, the plugin derives a rough local slug from the
prompt.

## What it renames

A prompt about reviewing a cache might rename the pane to `cache-review`. In an
auto-generated linked worktree, the plugin can also rename:

- branch: `<prefix>/cache-review`, or `cache-review` without a prefix
- workspace: `cache-review`

Branch and workspace renaming only happens when the current branch starts with
`worktree/`. The branch rename is local and never pushes to the remote.

The generated name is also published as a `task` metadata token on the pane and
workspace. This makes `$task` available to custom Agent and Space sidebar rows.
For example:

```toml
[ui.sidebar.agents]
rows = [["state_icon", "agent"], ["$task"]]

[ui.sidebar.spaces]
rows = [["workspace"], ["$task"]]
```

## Fork additions (multi-agent naming)

This fork extends upstream with:

- **More agents**: Pi, Grok, and opencode transcripts are parsed in addition
  to Claude Code and Codex. Grok has no herdr integration, so its session is
  recovered from `~/.grok/active_sessions.json` by matching the pane's
  foreground pid (cwd + newest-live as fallback); the prompt comes from the
  `<user_query>` block in `chat_history.jsonl`. Pi integrations may report the
  transcript path as the session value; that path is used directly.
- **More rename targets**: besides the pane, the generated name is applied to
  the herdr tab and the agent sidebar entry. Configure with
  `HERDR_NAMING_TARGETS` or a `targets` config file (comma-separated subset of
  `pane,tab,agent`; default all).
- **More naming engines**: headless `opencode run` and `claude -p` join
  FoundationModels and Codex. `HERDR_NAMING_ENGINE` (or an `engine` config
  file) now accepts a comma-separated chain, e.g. `opencode,claude` for
  machines where Apple Intelligence and direct Codex access are unavailable.
  Engine binaries are resolved from standard install paths when the herdr
  server runs with a minimal launchd PATH.
- **CJK-friendly fallback**: when every engine fails, pane/tab/agent labels
  keep a capped excerpt of the original (e.g. Chinese) prompt while the git
  branch slug stays ASCII.

| Additional setting | Default | Purpose |
| ------------------ | ------- | ------- |
| `HERDR_NAMING_TARGETS` (or `targets` file) | `pane,tab,agent` | Which herdr labels receive the name |
| `HERDR_NAMING_ENGINE` (or `engine` file) | platform chain | Single engine or comma-separated chain |
| `HERDR_NAMING_CLAUDE_BIN` / `HERDR_NAMING_CLAUDE_MODEL` | `claude` / unset | Claude engine binary and optional `--model` |
| `HERDR_NAMING_OPENCODE_BIN` | `opencode` | opencode engine binary |
| `HERDR_NAMING_GROK_DIR` | `~/.grok` | Grok home for session/transcript lookup |

Config files live in `$(herdr plugin config-dir herdr-plugin-renamer)/`.

## Configuration

All settings are optional.

| Setting                       | Default        | Purpose                                      |
| ----------------------------- | -------------- | -------------------------------------------- |
| `HERDR_NAMING_ENGINE`         | `foundation`   | Use Foundation with Codex fallback, or `codex` only |
| `HERDR_NAMING_BRANCH_PREFIX`  | none           | Prefix renamed branches, such as `wyattjoh`  |
| `HERDR_NAMING_FOUNDATION_BIN` | bundled helper | Override the FoundationModels helper path    |
| `HERDR_NAMING_CODEX_BIN`      | `codex`        | Override the Codex executable path           |

To configure a persistent branch prefix:

```sh
echo wyattjoh > "$(herdr plugin config-dir herdr-plugin-renamer)/branch-prefix"
```

`HERDR_NAMING_BRANCH_PREFIX` takes precedence over that file. Environment
variables must be available wherever herdr is launched.

## Local development

```sh
just build
just link
```

`herdr plugin link` does not run build steps, so use `just link` or build first.
See [CONTRIBUTING.md](CONTRIBUTING.md) for the complete development and test
workflow.

## License

[MIT](LICENSE)
