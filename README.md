# Agentree

**Agentree** is a Windows terminal dashboard for [Claude Code](https://code.claude.com/docs/en/overview) and its native Agent Teams. It gives you one place to choose a project, describe an objective, work with Claude, and watch the team make progress.

The dashboard displays teammates, current tool activity, elapsed time, token usage, approximate context usage, and estimated API-list-price costs. It keeps Claude's interactive terminal embedded, including permission requests and questions. Agentree uses **Haiku 5.5** for the team lead and workers, with **Opus 5.5** available for advice and final review.

Agentree is an observer and terminal host, **not a replacement for Claude Code**. Agent creation, delegation, advice, and review are handled by Claude. Some activity and usage information may be delayed or unavailable; cost and context figures are estimates.

## Install (Windows)

**Requirements:** [Claude Code](https://code.claude.com/docs/en/setup) (installed and signed in), [Rust/Cargo](https://rustup.rs/), and Windows 10/11. Windows Terminal is recommended. The [GitHub CLI](https://cli.github.com) (`gh`) is optional, for GitHub issue objectives; see [Troubleshooting](docs/TROUBLESHOOTING.md#github-issue-objectives).

Clone or download this repository, open PowerShell in the project folder, and run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

The installer runs the tests, builds Agentree, and installs the `agentree` command for your user. **Restart your terminal**, then run:

```powershell
agentree
```

The normal `claude` command is unchanged.

## Quick start

1. **Choose a project** — enter its folder path or press **F2** to browse.
2. **Enter an objective** — describe what you want Claude to build or change. Press **Ctrl+L** to clear it. Or press **F3** to pick an open GitHub issue as the objective (requires `gh`; see [Use a GitHub issue](#use-a-github-issue)).
3. **Start** — press **F5**. Claude starts in the embedded terminal and can delegate work to Haiku teammates.
4. **Follow progress** — use the agent-tree view for activity and usage; respond to Claude's prompts in the terminal.
5. **Review the result** — check Claude's summary, tests, and file changes before committing anything.

### Keyboard shortcuts

| Key | Action |
| --- | --- |
| **F2** | Browse project folders (setup screen) |
| **F3** | Open GitHub issues as the objective (setup screen) |
| **F5** | Start the objective |
| **Ctrl+L** | Clear the objective (setup screen) |
| **F6** | Focus Claude / return to split view |
| **F8** | Toggle full Claude terminal |
| **F9** | Toggle full agent tree |
| **F7** | Respond to a reported blocker |
| **F4** | Pause/resume the displayed timer |
| **Ctrl+Q** twice | Quit and terminate the embedded session |

#### Issues page keys (F3)

The Issues page is read-only. It lists open issues for the GitHub repository that `origin` of the selected project points to, with a preview pane.

| Key | Action |
| --- | --- |
| Type | Filter by title or number (for example `12` or `#12`) |
| **Backspace** | Edit the filter |
| **Up** / **Down** | Move one row |
| **PageUp** / **PageDown** | Move ten rows |
| **Enter** | Fill the objective with the selected issue and return to Setup |
| **Ctrl+R** | Reload the list |
| **Esc** | Cancel and return to Setup without changing the objective |

After **Enter**, review or edit the objective, then press **F5** to start.

You can also preview the interface without starting Claude using `agentree --demo`.

## Use a GitHub issue

Pass an issue number with `--project` to load it at startup:

```powershell
agentree --project "C:\path\to\repo" --issue 38
```

This places the generated objective in Setup, focused for review. It does not start Claude. `--issue` requires `--project` and cannot be combined with `--objective`. It needs the GitHub CLI (`gh`) installed and signed in; see [Troubleshooting](docs/TROUBLESHOOTING.md#github-issue-objectives).

For implementation details, see [Development](docs/DEVELOPMENT.md). For common setup issues, see [Troubleshooting](docs/TROUBLESHOOTING.md).
