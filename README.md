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
| **F4** | Pause/resume the displayed timer (running); open the role model settings (setup screen) |
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

## Role models and profiles

Press **F4** on the setup screen to open the Roles overlay. It sets the model, effort, concurrency and permission mode for each role: Lead, Planner, Explorer, Researcher, Coder, Tester, Advisor and Final reviewer. Changes are saved at once to `profiles.json` in `%LOCALAPPDATA%\ClaudeAgentTree`, so they survive a restart.

### Profiles

Settings are kept in named profiles. Four are built in:

| Profile | What it is |
| --- | --- |
| **Balanced** | The default, and the launch Agentree has always used: Haiku lead and teammates, up to three of each teammate role, Opus advisor and final review. |
| **Budget** | Haiku throughout, lead effort low, one teammate per role. |
| **Local** | Reserved for local models, which arrive in v0.7. Until then it is Haiku only with no advisor, so Opus is never called and a final review can only end BLOCKED. |
| **Thorough** | Sonnet for the lead, Planner, Coder and Tester, lead effort high. |

You can edit any profile; **d** restores a role to its built-in values (Balanced's, for a profile that is not built in).

### Per-project choices

A project can pin a different profile and override single fields over it. The settings a run uses are resolved in this order, later steps winning:

1. the active (global) profile,
2. the profile pinned for the project, if any,
3. the project's field overrides.

Project choices are saved in the same `profiles.json` under `%LOCALAPPDATA%\ClaudeAgentTree`, keyed by the project's folder. Nothing is written to the project directory or the repository, so a cloned repository cannot change models or permission modes. In the overlay, a `*` after a role marks it as overridden for the current project.

| Key | Action |
| --- | --- |
| **Up** / **Down** | Choose a role |
| **Left** / **Right** | Change the selected value |
| **Tab** | Move to the next field |
| **p** / **P** | Next / previous profile (the global one, or this project's pin in project scope; project scope also offers "follow global") |
| **s** | Switch between editing the profile and this project's overrides |
| **d** | Reset the selected role: to its built-in values, or in project scope clear its overrides |
| **Esc** or **F4** | Close the overlay |

### Upgrading from `roles.json`

The first time a version with profiles starts and finds a `roles.json`, its settings become the **Migrated** profile and it is made active, so a run is configured exactly as before. Unsupported values are replaced with defaults and listed in the overlay. `roles.json` is left in place and never written again. A `profiles.json` that is not valid or comes from a newer Agentree is moved to `profiles.json.invalid` (or `.invalid.1`, `.invalid.2`, and so on, never replacing an earlier backup) in `%LOCALAPPDATA%\ClaudeAgentTree`, and the built-in profiles are used. If the file cannot be moved aside or cannot be read, it is left exactly as it is, the overlay says so, and changes last only for that session and are not saved until you fix or move the file and restart.

### Credentials

Profiles may carry credential **references**, never values: `env:NAME` for an environment variable or `wincred:TARGET` for a Windows Credential Manager entry. Anything else, including a pasted API key, is rejected, removed on load and not shown in messages. Claude Code uses its own sign-in, so nothing reads these references in this version; they are the contract for backends that need credentials in v0.7. Edit `profiles.json` to add them:

```json
"credentials": { "anthropic": "env:ANTHROPIC_API_KEY" }
```

### How settings reach Claude

Each setting is labelled by how it reaches Claude:

- **enforced** settings are passed to Claude Code as launch flags: the lead's model, effort and permission mode, and the advisor.
- **instruction to lead** settings are written into the lead's prompt: teammate models, concurrency and permission modes. Claude Code does not apply these to each teammate, so Agentree cannot confirm them. Not verified in a live session.

With the advisor set to **off**, Opus is not consulted, so a final review can only end BLOCKED.

Not in this version: custom profile creation from the overlay (add one by editing `profiles.json`), and the `bypassPermissions`, `auto`, `dontAsk` and `manual` permission modes. Claude Code is the only backend. Pi and local models are planned for v0.7.

## Use a GitHub issue

Pass an issue number with `--project` to load it at startup:

```powershell
agentree --project "C:\path\to\repo" --issue 38
```

This places the generated objective in Setup, focused for review. It does not start Claude. `--issue` requires `--project` and cannot be combined with `--objective`. It needs the GitHub CLI (`gh`) installed and signed in; see [Troubleshooting](docs/TROUBLESHOOTING.md#github-issue-objectives).

For implementation details, see [Development](docs/DEVELOPMENT.md). For common setup issues, see [Troubleshooting](docs/TROUBLESHOOTING.md).
