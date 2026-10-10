# Development notes

Agentree is a native Windows Rust terminal application. It uses `ratatui` and `crossterm` for rendering and input, `portable-pty`/Windows ConPTY to host a real interactive Claude Code session, and `vt100` to interpret terminal output.

## Run from source

Prerequisites: Windows, an installed and authenticated Claude Code CLI, Git, and Rust/Cargo.

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

## Components

| Source | Purpose |
| --- | --- |
| `src/main.rs` | CLI options, application lifecycle, input and objective timers |
| `src/ui.rs` | Terminal layout, panes and activity highlights |
| `src/pty.rs` | Native Claude subprocess and ConPTY integration |
| `src/team.rs` | Native team discovery and shared-task status |
| `src/telemetry.rs` | Local JSONL transcript parsing, usage and cost estimates |
| `src/live.rs` | Observed terminal activity and review/completion signals |
| `src/prompt.rs` | Team and escalation instructions given to Claude |

Claude itself coordinates teammates and handles permissions. Agentree does **not** directly implement task scheduling or a JEV/System One reasoning router. Native team status comes from Claude's local team/task files. Background subagents are not presented as confirmed native teammates.

Token and activity data come from local Claude session transcripts. The parser deduplicates repeated message usage where possible. Runtime reflects observation and objective lifecycle, not billed model-processing time. Estimated cost is based on API list prices; it is not an invoice or a calculation of Claude subscription usage. Context utilisation is approximate.

Opus advice and final-review requests are model-directed, not externally enforced gates. The review panel distinguishes a reported review result from evidence that Opus was actually consulted. Claude's terminal remains the authoritative place for permission prompts and human decisions.

## Publishing

This is an executable Rust project, so commit the generated `Cargo.lock` for reproducible builds. Run `cargo generate-lockfile` on a machine with Cargo if it is not already present, and include it in the Git commit. Do not commit local Claude session logs, transcripts, credentials, `.env` files, or user projects.

The package declares `MIT` in `Cargo.toml` and includes a matching `LICENSE` file. Review the licence choice before publishing.
