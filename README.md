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
| **F1** | Open or close the controls panel (every page) |
| **Tab** | Switch between project path and objective (setup screen) |
| **F2** | Browse project folders (setup screen) |
| **F3** | Open GitHub issues as the objective (setup screen) |
| **Ctrl+L** | Clear the objective (setup screen) |
| **F5** | Start the objective (setup screen); cycle dashboard panels (running) |
| **F6** | Focus Claude / return to split view |
| **F7** | Respond to a reported blocker (running); show the Claude Code capability matrix (setup screen) |
| **F8** | Toggle full Claude terminal |
| **F9** | Toggle full dashboard |
| **F4** | Pause/resume the displayed timer |
| **Esc** | Quit (setup screen) |
| **Ctrl+Q** twice | Quit and terminate the embedded session |

Scrolling uses the mouse wheel and the **Ctrl** keys; see [Scrolling and controls](#scrolling-and-controls).

#### Issues page keys (F3)

The Issues page is read-only. It lists open issues for the GitHub repository that `origin` of the selected project points to, with a preview pane.

| Key | Action |
| --- | --- |
| Type | Filter by title or number (for example `12` or `#12`) |
| **Backspace** | Edit the filter |
| **Up** / **Down** | Move the selection one row; the list follows it |
| **PageUp** / **PageDown** | Move the selection ten rows; the list follows it |
| **Tab** | Switch Ctrl+scroll between the list and the preview |
| **Enter** | Fill the objective with the selected issue and return to Setup |
| **Ctrl+R** | Reload the list |
| **Esc** | Cancel any pending load and return to Setup without changing the objective |

After **Enter**, review or edit the objective, then press **F5** to start.

### Scrolling and controls

Every panel scrolls the same way:

| Input | Effect |
| --- | --- |
| Mouse wheel | Scroll the panel under the pointer by 3 lines. Does not change focus or selection. |
| **Ctrl+Up** / **Ctrl+Down** | Scroll the focused panel one line |
| **Ctrl+PageUp** / **Ctrl+PageDown** | Scroll the focused panel one page |
| **Ctrl+Home** / **Ctrl+End** | Go to the top / bottom of the focused panel |

The focused panel depends on the page:

- **Setup:** the objective while you are editing it, otherwise the project path.
- **Folder browser (F2):** the folder list.
- **GitHub issues (F3):** the issue list, or the preview after **Tab**.
- **Running:** the Claude terminal, the dashboard panel selected with **F5**, or the NEEDS ATTENTION overlay when it is open.

A panel whose content overflows shows its position in the title, for example `↓ 1-10/40`. The `↑` and `↓` arrows show which way more content lies. Ctrl+scroll keys are never forwarded to Claude.

**F1** opens the controls panel on every page. It scrolls with the same keys, and **Esc** or **F1** closes it.

#### Claude terminal history

- The Claude terminal keeps up to 2,000 lines of scrollback.
- Scroll back with the mouse wheel or Ctrl+scroll. While you read history, the view does not jump to the bottom when new output arrives.
- The title reads `history -N` while you are scrolled back. **Ctrl+End** or typing any key returns to the live screen.
- Claude running in its own fullscreen mode (alternate screen) has no scrollback. The mouse wheel is forwarded to Claude only when Claude has enabled mouse reporting; otherwise the wheel does nothing there.
- Agentree captures the mouse while it runs, so click-drag text selection inside Agentree is disabled. To select text, use your terminal's bypass modifier (Windows Terminal: **Shift+drag**).

You can also preview the interface without starting Claude using `agentree --demo`.

## Use a GitHub issue

Pass an issue number with `--project` to load it at startup:

```powershell
agentree --project "C:\path\to\repo" --issue 38
```

This places the generated objective in Setup, focused for review. It does not start Claude. `--issue` requires `--project` and cannot be combined with `--objective`. It needs the GitHub CLI (`gh`) installed and signed in; see [Troubleshooting](docs/TROUBLESHOOTING.md#github-issue-objectives).

For implementation details, see [Development](docs/DEVELOPMENT.md). For common setup issues, see [Troubleshooting](docs/TROUBLESHOOTING.md).
