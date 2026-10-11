# Troubleshooting

## `agentree` is not recognised

Restart Windows Terminal after installation to refresh the user PATH. Then check:

```powershell
Get-Command agentree -All
```

The installed command should resolve to `%LOCALAPPDATA%\ClaudeAgentTree\bin\agentree.exe`. You can rerun `install.ps1` from the source directory if needed.

## Claude starts but the embedded terminal stays blank

Confirm that the normal Claude CLI works in the same project directory:

```powershell
claude
```

Complete any folder-trust prompt, exit Claude, and retry Agentree. If it still stalls, see the session's `startup-status.json` under `%LOCALAPPDATA%\ClaudeAgentTree\runs\`. The installer also supports `-ClaudeCli "C:\path\to\claude.exe"` if you need to select a different Claude installation.

## Launch blocked by a capability check

Pressing **F5** shows `Launch blocked: ...` when the installed Claude Code CLI lacks a flag Agentree needs, or the launch plan names an unknown model. Nothing is saved or started. The capability matrix opens automatically; press **F7** on the setup screen to see it again.

Cause: most often an older Claude Code build. Hidden flags such as `--advisor` and `--teammate-mode` are not listed in `claude --help`, so check the matrix rather than the help text.

Fix: run `claude update`, confirm `claude --version`, then press **F5** again. If several CLIs are installed, `CLAUDE_TREE_REAL_CLAUDE` or `install.ps1 -ClaudeCli` selects the one Agentree uses. The matrix shows which path was probed.

`unverified` rows in the matrix are not failures. They can only be confirmed in a live session.

## Role settings

**Reset a role:** open the Roles overlay with **F4** on the setup screen, select the role, and press **d**. This restores that role to its built-in values, or, in project scope (**s**), clears that role's overrides for the project.

**Reset everything:** close Agentree and delete `%LOCALAPPDATA%\ClaudeAgentTree\profiles.json`. The built-in profiles are used. If an old `roles.json` is still there it is migrated again, so delete it too for a clean start.

**Which settings a run uses:** the overlay header names the profile in use and whether this project pins it. Project overrides are marked `*` and win over the profile. Choices made for one project do not affect others.

**Warnings in the overlay:**

- `Profile <name>: <Role> <setting> '<value>' is not supported; using <default>.` A value in `profiles.json` was edited by hand to one the overlay does not offer. Agentree uses the default for that setting. Choose the value again in the overlay, or fix the file.
- `Project <path>: ... ignored.` or `... pins profile '<name>', which does not exist` A project's saved choice is no longer valid. It is ignored.
- `Profile <name>: credential '<label>' is not a reference ... and was removed.` A credential must be `env:NAME` or `wincred:TARGET`. A plaintext value was refused and is not saved again.
- `profiles.json could not be used (...)` The file is not valid JSON or comes from a newer Agentree. It was moved to `profiles.json.invalid` (or `.invalid.1`, and so on, if one exists) and the built-in profiles are used. Fix and rename it back, or keep working: the next change writes a new `profiles.json`.
- `... could not be moved aside (...)` or `... could not be read (...)` Agentree could not preserve `%LOCALAPPDATA%\ClaudeAgentTree\profiles.json` (it is open in another program, or access is denied). The file was left unchanged and changes in this session are not saved. Close whatever holds it, or move or fix the file yourself, then restart Agentree.
- `Your role settings from roles.json are now the Migrated profile.` One-time notice after upgrading. `roles.json` was left in place.
- `roles.json could not be read (...)` The old file could not be parsed, so it was not migrated and was left untouched.

**`NOT SUPPORTED` for Effort level or Permission modes (F7):** the installed Claude Code CLI rejects `--effort` or `--permission-mode`. The launch is blocked only when the lead's effort or permission mode is set to something other than default, because only then does the plan need the flag. Fix: run `claude update`, then press **F5** again, or set the lead's effort and permission mode back to default. Not verified in a live session.

Teammate models, concurrency limits and permission modes are instructions to the lead, not launch flags. Agentree cannot confirm that a teammate follows them. A teammate that ignores one is not a launch failure; check the Claude terminal.

## Teammates are missing from the dashboard

Claude may run named teammates, background subagents, or no parallel workers, depending on the task and available tools. Agentree only links a native team when the local Claude records match the active project/session with sufficient confidence. Check the embedded Claude terminal before assuming work has stopped.

## Permission request or blocker

Press **F6** or **F8** to operate the real Claude terminal. Native permission questions must be answered there. Press **F7** only when Agentree has detected a structured blocker.

## Timer, usage or review status looks wrong

The activity log depends on records emitted by Claude, which may arrive late. Some Opus advisor usage is not present in the local transcript and is marked unavailable instead of estimated. The objective timer stops when Claude emits a completion signal; **F4** allows a manual pause/resume when that signal is absent.

## GitHub issue objectives

These problems affect **F3** (the Issues page) and `--issue`. Agentree reads issues through the GitHub CLI (`gh`) and never changes them. Nothing here affects normal objectives.

### `gh` not found on PATH

Symptom: "GitHub CLI (`gh`) was not found on PATH."

Cause: the GitHub CLI is not installed, or the terminal was opened before it was added to PATH.

Fix: install it from [cli.github.com](https://cli.github.com), restart Windows Terminal, then check with `Get-Command gh`.

### GitHub CLI not signed in

Symptom: "GitHub CLI is not signed in."

Cause: `gh` has no authenticated account.

Fix: run `gh auth login`, then check the account with `gh auth status`.

### Repository not found or not accessible

Symptom: "owner/repo was not found or is not visible to the account signed in to gh. Private repositories need access."

Cause: the `origin` URL points to a repository that does not exist, or the signed-in account cannot read it. Private repositories need an account with access.

Fix: check `gh auth status` for the account in use, and sign in with an account that can read the repository (`gh auth login`). Confirm the repository with `gh repo view owner/name`.

### Issue number not found

Symptom: "Issue #N was not found."

Cause: the number does not exist in the repository that `origin` points to, or it belongs to another repository.

Fix: check the number against the repository's issue list on GitHub. `--issue` accepts issues in any state, but F3 lists only open issues.

### Network or proxy errors

Symptom: "Could not reach GitHub. Check your network connection or proxy and try again."

Cause: Agentree cannot reach GitHub from this machine.

Fix: check the internet connection and any proxy settings, then press **Ctrl+R** on the Issues page to reload.

### Git "dubious ownership"

Symptom: "git failed: fatal: detected dubious ownership in repository at '...'"

Cause: Git refuses to read a repository owned by another Windows user, so Agentree cannot find its `origin` remote.

Fix: trust the folder for your user, then retry:

```powershell
git config --global --add safe.directory <path>
```

### Non-GitHub, missing or ambiguous `origin` remote

Symptoms, depending on the problem:

- "No 'origin' remote (found: ...)"
- "origin points at different repositories (...). Keep one URL in origin."
- "origin points at <host>, not github.com."
- "The project folder is not a Git repository."

Cause: Agentree only uses the `origin` remote, and only github.com remotes. The folder may not be a Git repository, may have no `origin`, may point to a non-GitHub host, or may have several URLs in `origin` that name different repositories. Push-specific URLs are not checked.

Fix: run `git -C <project> remote -v` to see the remotes, and `git -C <project> remote get-url --all origin` to list every URL `origin` uses. Keep one GitHub URL with `git remote set-url origin <url>`, using HTTPS (`https://github.com/owner/repo.git`) or SSH (`git@github.com:owner/repo.git`).

## Uninstall

From the Agentree source folder:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall-shim.ps1
```

This removes Agentree's user-level command without uninstalling Claude Code.
