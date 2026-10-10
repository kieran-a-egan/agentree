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

## Teammates are missing from the dashboard

Claude may run named teammates, background subagents, or no parallel workers, depending on the task and available tools. Agentree only links a native team when the local Claude records match the active project/session with sufficient confidence. Check the embedded Claude terminal before assuming work has stopped.

## Permission request or blocker

Press **F6** or **F8** to operate the real Claude terminal. Native permission questions must be answered there. Press **F7** only when Agentree has detected a structured blocker.

## Timer, usage or review status looks wrong

The activity log depends on records emitted by Claude, which may arrive late. Some Opus advisor usage is not present in the local transcript and is marked unavailable instead of estimated. The objective timer stops when Claude emits a completion signal; **F4** allows a manual pause/resume when that signal is absent.

## Uninstall

From the Agentree source folder:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall-shim.ps1
```

This removes Agentree's user-level command without uninstalling Claude Code.
