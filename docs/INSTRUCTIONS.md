# Project instructions: AGENTS.md and CLAUDE.md

Agentree resolves the project instruction files for a run with `src/instructions.rs`. It only reads them: no instruction file is created, merged, rewritten or overwritten.

## Precedence (Claude backend)

Lowest to highest: `AGENTS.md` (shared, portable), then `CLAUDE.md` (Claude-specific). Claude Code loads `CLAUDE.md` itself; Agentree does not rely on it loading `AGENTS.md`, so when `AGENTS.md` is present and not already pulled in, the lead's standing instructions gain a short note naming it. Only the project folder is resolved; nested and user-level files stay Claude Code's own. `GEMINI.md`, `.github/copilot-instructions.md` and `.cursorrules` are listed for provenance and not used by the Claude backend.

| Files present | Result | Lead prompt |
| --- | --- | --- |
| neither | no project instructions | unchanged |
| `CLAUDE.md` only | loaded natively | unchanged |
| `AGENTS.md` only | lead is told to read it | note added |
| same file, identical content, or `CLAUDE.md` has a line that is exactly `@AGENTS.md` | nothing to reconcile | unchanged |
| both, different content | conflict explained: line counts unique to each file; `CLAUDE.md` wins | note added |

To remove a conflict, add `@AGENTS.md` to `CLAUDE.md` (see [`examples/instructions`](../examples/instructions)) or keep one file.

## Provenance

```powershell
agentree --instructions C:\path\to\project
```

At launch the status bar shows a line when `AGENTS.md` needs attention (alone, conflicting or unreadable). `--instructions` prints each file found, the result and, for a conflict, why. Every launch writes the same text to `instructions-resolved.txt` in the run folder next to `instructions.txt`.
