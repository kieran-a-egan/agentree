# Development notes

Agentree is a native Windows Rust terminal application. It uses `ratatui` and `crossterm` for rendering and input, `portable-pty`/Windows ConPTY to host a real interactive Claude Code session, and `vt100` to interpret terminal output.

## Run from source

Prerequisites: Windows, an installed and authenticated Claude Code CLI, Git, and Rust/Cargo. The GitHub CLI (`gh`) is optional and is needed only for GitHub issue objectives.

```powershell
cargo test
cargo build --release
.\target\release\claude-agent-tree.exe
```

To install or update the user-level `agentree.exe` command:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -SkipBuild
```

To remove the installed command without affecting Claude Code:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall-shim.ps1
```

## Continuous integration and regression coverage

Pull requests to `main` and pushes to `main` run [Windows CI](../.github/workflows/windows-ci.yml) on `windows-latest` using stable Rust. The required checks are:

```powershell
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
.\target\release\claude-agent-tree.exe --version
.\target\release\claude-agent-tree.exe --help
```

Use the checked-in `Cargo.lock`; dependency changes must update it intentionally. No Claude credentials or external API keys are installed on the CI runner. Tests cover the Rust contracts, Claude team association and non-interactive ConPTY/launcher invariants. In particular, the PTY tests exercise session arguments, native team environment settings, terminal response chunking, and on Windows the ability to create a ConPTY, launch a benign child process, and request its termination. Automatic subprocess exit and actual Claude terminal interaction require the manual smoke test. They do **not** constitute a live authenticated Claude Agent Teams integration test.

Before merging a change to the launcher or terminal UI, additionally smoke-test **on a developer's Windows machine with Claude signed in**: launch `agentree`, start an objective, verify the terminal accepts input and permission prompts, observe a named teammate in the dashboard, and ensure review/completion markers and exit cleanup still work. GitHub CI can't validate those third-party interactive behaviours without live credentials.

## v0.5 release gate

Automated, run by CI: `capability::tests::release_gate_model_change_persists_and_launches` saves a role model to `profiles.json`, reloads it as a restart would, and checks the model reaches the launch flags and teammate instructions and that unsupported values fail `preflight`. `roles_overlay_*` tests in `src/main.rs` cover the UI edit-and-save path, and `profiles::tests` cover migration from `roles.json`.

Manual, on a Windows machine with Claude signed in (CI has no credentials):

1. `cargo test --locked -- --ignored --nocapture installed_cli` prints the capability matrix and passes.
2. Start `agentree`, press **F4**, change the Lead model, close the overlay, quit and restart. The overlay shows the saved model.
3. Start an objective. The Claude session starts with the chosen model, and teammates appear in the dashboard.
4. Choose a setting the installed Claude Code does not accept (for example by running an older CLI). **F5** opens the capability matrix and nothing is launched.

## Components

| Source | Purpose |
| --- | --- |
| `src/main.rs` | CLI options, application lifecycle, input and objective timers |
| `src/ui.rs` | Terminal layout, panes, mouse hit-testing regions and activity highlights |
| `src/scroll.rs` | Shared scroll model: pane identity, offsets, key and wheel mapping |
| `src/pty.rs` | Native Claude subprocess and ConPTY integration |
| `src/capability.rs` | Claude Code capability matrix, launch-plan preflight and the flag probes behind them |
| `src/roles.rs` | Per-role model settings: the Roles overlay options, validation, launch notes and the launch plan they produce |
| `src/profiles.rs` | Saved profiles, per-project overrides, credential references and `roles.json` migration (`profiles.json`) |
| `src/team.rs` | Native team discovery and shared-task status |
| `src/telemetry.rs` | Local JSONL transcript parsing, usage and cost estimates; emits core events |
| `src/model.rs` | Runtime-neutral `Run`, `Agent`, `Task`, `Stage`, `Usage`, `Approval` and `Event` types |
| `src/live.rs` | Observed terminal activity and review/completion signals |
| `src/prompt.rs` | Team and escalation instructions given to Claude |
| `src/github.rs` | GitHub remote discovery, read-only `gh` issue loading, and issue-to-objective generation |

The versioned event contract, including how unknown usage is represented, is documented in `docs/EVENT_SCHEMA.md`. Claude-specific parsing belongs in the adapters (`telemetry.rs`, `team.rs`); new dashboard code should consume `model` types rather than Claude file formats.

### Capability discovery and launch preflight

`src/capability.rs` owns what Agentree asks Claude Code to run. A `RolePlan` (lead model, subagent model, advisor, native teams) produces the launch flags and environment in `pty.rs`, and the same plan is checked by `preflight` before anything is saved or spawned. A plan that passes preflight is the plan that launches.

Discovery runs each probed flag once with no value (`claude --advisor`). A CLI that knows the flag fails with `argument missing`; one that does not fails with `unknown option`. Neither starts a session, and the probes run in parallel (about 0.15 s). Anything else is `Unverified`.

- **Blocks launch:** a proven `Unsupported` flag that the plan needs (`--model`, `--session-id`, `--append-system-prompt-file`, plus `--advisor` when an advisor is set and `--teammate-mode` when native teams are on), or a model name outside the known list (`claude-haiku-5-5`, `claude-sonnet-5-5`, `claude-opus-5-5`) or an advisor other than `opus`.
- **Never blocks:** `Unverified` results. Environment variables such as `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` and account model access can only be confirmed in a live session, so they are listed as unverified rather than refused.

To check the rules against the Claude Code installed on your machine, run `cargo test -- --ignored --nocapture installed_cli`. The test prints the matrix and fails if the default plan is blocked. It is ignored in CI because the runner has no Claude CLI.

The matrix is shown with **F7** on the setup screen and opens automatically when a launch is blocked. Each probe is re-run on every check, so updating Claude Code and pressing F5 again takes effect without restarting Agentree. To add a probed flag, add it to `PROBED` and to the plan rule that needs it, then extend `preflight`'s tests.

The `--advisor` value is the alias `opus`, not a full model name. Keep the alias in `RolePlan` and in `ADVISORS`.

Claude itself coordinates teammates and handles permissions. Agentree does **not** directly implement task scheduling or a JEV/System One reasoning router. Native team status comes from Claude's local team/task files. Background subagents are not presented as confirmed native teammates.

Token and activity data come from local Claude session transcripts. The parser deduplicates repeated message usage where possible. Runtime reflects observation and objective lifecycle, not billed model-processing time. Estimated cost is based on API list prices; it is not an invoice or a calculation of Claude subscription usage. Context utilisation is approximate.

Opus advice and final-review requests are model-directed, not externally enforced gates. The review panel distinguishes a reported review result from evidence that Opus was actually consulted. Claude's terminal remains the authoritative place for permission prompts and human decisions.

### Role settings

`src/roles.rs` owns the per-role settings: the `Settings` that the Roles overlay edits (one per profile in `profiles.json`), the values each field can take, validation on load, the launch notes, and `Settings::plan()`, which builds the `RolePlan`. `capability.rs` owns `RolePlan` and `preflight`. `profiles.rs` stores `Settings` per profile and resolves a project's effective `Settings`. The overlay, the file and the launch all read from those, so a saved choice is the one that launches.

`roles::options(role, field)` is the single source of truth for the values a field can take. The overlay cycles through it, `Settings::sanitized` replaces any stored value outside it with the default, and `roles::applies` uses it to decide whether a field applies to a role. An empty slice means the field does not apply.

Settings reach Claude in two ways, and `roles::applies` labels each one:

- **Enforced** by a launch flag: the lead model (`--model`), lead effort (`--effort`), lead permission mode (`--permission-mode`), and the advisor (`--advisor`, omitted when off). Unnamed subagents use the Coder model through `CLAUDE_CODE_SUBAGENT_MODEL`.
- **Instruction to lead** only: teammate models, concurrency limits and permission modes. `Settings::notes` says Agentree cannot confirm them. Do not describe them as enforced.

Preflight checks the effort and permission values against the same lists in `roles.rs`, and blocks only when the plan sets a flag the CLI rejects. The final reviewer is labelled "via the Opus advisor" because it runs through the advisor.

To add an option:

1. Add the value to the matching list in `roles.rs` (`MODELS`, `EFFORTS`, `PERMISSIONS`, `CONCURRENCY` or `ADVISOR_MODELS`), or change `options` if only some roles get it. Add a `model_label` entry for a new model.
2. If the value is a launch flag that is new to Agentree, add the flag to `PROBED` in `capability.rs` and to the `needed` list in `preflight` for the plan rule that uses it.
3. If the value is an instruction, carry it in `Settings::plan()` (the `Teammate` entries), render it in `system_policy` in `prompt.rs` under "Role assignments", and mention it in `notes`.
4. Extend the `roles.rs` tests, and the `capability.rs` tests when a flag is involved.

## Terminal stack

- `ratatui` 0.30, with the `unstable-rendered-line-info` feature. The feature provides `Paragraph::line_count`, which `src/ui.rs` uses to clamp pane scroll offsets to wrapped content.
- `crossterm` 0.29 for input and terminal control.
- `vt100` 0.16.2, pinned exactly (`=0.16.2`) in `Cargo.toml`. This replaces 0.15.2, whose `visible_rows` underflows (panics in debug builds) when the scrollback offset exceeds the screen height, which is needed to scroll back past one screen.

## Scrolling architecture

- `src/scroll.rs` owns the shared model.
  - `Pane` names every scrollable pane. `Pane::DASHBOARD` is the order F5 cycles through.
  - `View` holds the offset for one pane. Renderers clamp offsets and record the bounds in `Cell`s, so input handlers, which only hold `&App`, can clamp against what was last drawn. A `tail` view (the session log) anchors to the newest line: its stored offset is the distance from the end, so dropped old lines do not shift the reader's position.
  - `Move` (`Lines`, `Pages`, `Top`, `End`) is the single request type. `key_move` maps the Ctrl+arrow, Ctrl+page and Ctrl+Home/End chords to it, and `WHEEL_LINES` (3) is the wheel step. `target` is a pure function that computes the new top and clamps it to the bounds.
- `src/ui.rs`: `regions` lays out every pane, and `pane_at` hit-tests a mouse position against that same layout. Draw and mouse handling share one layout, so a wheel event always lands on the panel the user sees.
- Claude's terminal history lives in vt100, not in Agentree. Scrollback is read with `screen().scrollback()` and set with `set_scrollback`; the 2,000-line bound is set in `src/main.rs`. Code that needs the live screen (for example `live_text` in `src/main.rs`) sets the offset to 0, reads, and restores the previous offset.

## Manual Windows validation checklist

Run these on a Windows machine with Windows Terminal and Claude signed in, before merging a change to scrolling, layout or controls. Automated tests do not cover them.

- [ ] Mouse wheel scrolls each panel under the pointer (Setup project and objective, folder browser, issue list and preview, each dashboard panel, controls panel) and does not change focus or selection.
- [ ] Ctrl+Up/Down, Ctrl+PageUp/PageDown and Ctrl+Home/End scroll the focused panel on each page.
- [ ] While Claude streams output, scroll the terminal back with the wheel: the view stays put and the title shows `history -N`.
- [ ] Ctrl+End, and typing any key, return to the live screen.
- [ ] Resize the window while scrolled in a panel and in the terminal: offsets stay clamped and nothing renders out of range.
- [ ] F1 opens the controls panel on every page; Esc and F1 close it.
- [ ] F5 cycles dashboard panels on the Running page, and Ctrl+scroll follows the selected panel.
- [ ] Ctrl+scroll chords do not reach Claude: Claude's input line is unchanged after Ctrl+Up/Down.
- [ ] In fullscreen Claude (alternate screen), the wheel does nothing unless Claude has enabled mouse reporting.
- [ ] After F3 adopts a long issue, the objective review starts at the top and scrolls through the whole text.

## Publishing

This is an executable Rust project, so commit the generated `Cargo.lock` for reproducible builds. Run `cargo generate-lockfile` on a machine with Cargo if it is not already present, and include it in the Git commit. Do not commit local Claude session logs, transcripts, credentials, `.env` files, or user projects.

The package declares `MIT` in `Cargo.toml` and includes a matching `LICENSE` file. Review the licence choice before publishing.

## Settings file

`%LOCALAPPDATA%\ClaudeAgentTree\profiles.json` (format `version` 1) holds `active` (the global profile name), `profiles` (name to `{ roles, credentials }`, where `roles` has the shape of `roles::Settings`) and `projects` (a normalised folder key, from `profiles::project_key`, to an optional pinned `profile` and sparse per-role `roles` overrides). `Store::effective` resolves active profile, then pin, then overrides. Every value is checked against `roles::options` on load; unsupported values are replaced and reported. Credentials are `CredentialRef` values only. Writes go through a temporary file and a rename.

`roles.json` is read once, by `profiles::load`, when `profiles.json` does not exist, and is never written. Under `cargo test` the data folder is a per-process temporary directory, so tests cannot touch real settings.
