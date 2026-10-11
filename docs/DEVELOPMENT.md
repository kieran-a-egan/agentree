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

## Components

| Source | Purpose |
| --- | --- |
| `src/main.rs` | CLI options, application lifecycle, input and objective timers |
| `src/ui.rs` | Terminal layout, panes and activity highlights |
| `src/pty.rs` | Native Claude subprocess and ConPTY integration |
| `src/team.rs` | Native team discovery and shared-task status |
| `src/telemetry.rs` | Local JSONL transcript parsing, usage and cost estimates; emits core events |
| `src/model.rs` | Runtime-neutral `Run`, `Agent`, `Task`, `Stage`, `Usage`, `Approval` and `Event` types |
| `src/live.rs` | Observed terminal activity and review/completion signals |
| `src/prompt.rs` | Team and escalation instructions given to Claude |
| `src/github.rs` | GitHub remote discovery, read-only `gh` issue loading, and issue-to-objective generation |

The versioned event contract, including how unknown usage is represented, is documented in `docs/EVENT_SCHEMA.md`. Claude-specific parsing belongs in the adapters (`telemetry.rs`, `team.rs`); new dashboard code should consume `model` types rather than Claude file formats.

Claude itself coordinates teammates and handles permissions. Agentree does **not** directly implement task scheduling or a JEV/System One reasoning router. Native team status comes from Claude's local team/task files. Background subagents are not presented as confirmed native teammates.

Token and activity data come from local Claude session transcripts. The parser deduplicates repeated message usage where possible. Runtime reflects observation and objective lifecycle, not billed model-processing time. Estimated cost is based on API list prices; it is not an invoice or a calculation of Claude subscription usage. Context utilisation is approximate.

Opus advice and final-review requests are model-directed, not externally enforced gates. The review panel distinguishes a reported review result from evidence that Opus was actually consulted. Claude's terminal remains the authoritative place for permission prompts and human decisions.

## Publishing

This is an executable Rust project, so commit the generated `Cargo.lock` for reproducible builds. Run `cargo generate-lockfile` on a machine with Cargo if it is not already present, and include it in the Git commit. Do not commit local Claude session logs, transcripts, credentials, `.env` files, or user projects.

The package declares `MIT` in `Cargo.toml` and includes a matching `LICENSE` file. Review the licence choice before publishing.
