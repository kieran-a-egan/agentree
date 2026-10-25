# Project instructions: AGENTS.md and CLAUDE.md

Agentree resolves the project instruction files for a run with `src/instructions.rs`. It only reads them: no instruction file is created, merged, rewritten or overwritten. Instruction text is repository content: it is never promoted to Agentree policy and cannot override Git-write authorization, plugin trust or tool permissions.

## How `AGENTS.md` reaches Claude Code

Claude Code's own behaviour varies by build, settings and a remote feature flag, so Agentree separates what it checked from what it can only ask for:

| Result | Meaning | Lead prompt |
| --- | --- | --- |
| **dropped** | your settings drop project instruction files (`managed-only` / `none`). Checked first: it also wins over an import or a same-file link | none |
| **excluded** | a `claudeMdExcludes` pattern matches `AGENTS.md`, **or** a pattern could not be evaluated (unsupported syntax, too long, or beyond the caps below) and so might. Fail safe: no request, and a `Problem:` line names the pattern | none |
| **same file** | `AGENTS.md` and a non-excluded `CLAUDE.md`-family file are one file | none |
| **imported** | a non-excluded `CLAUDE.md`-family file (`CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`) has an `@import` chain reaching `AGENTS.md` within Claude Code's hop limit. This is the import *syntax*; Claude's loading is not observed, so it is **not confirmed delivery** | none |
| **natively configured** | your Claude Code *user* settings make it load `AGENTS.md` (`instructionFiles`: `claude-md-and-agents-md`, or `claude-md-or-agents-md` with no `CLAUDE.md` in or above the project). **Not verified**: command-line `--settings`, the build and its feature flag are not checked | conditional note: read it *only if* its content is not already loaded |
| **unverified** | no setting found and no `CLAUDE.md` in the project (or one above it that may switch the fallback off), **or** the `@import` search hit a read limit or an unreadable file so neither an import nor its absence is established | same conditional note |
| **requested** | `CLAUDE.md` exists and no import was found, or settings say `claude-md` only. Managed and command-line settings are not inspected, so Claude Code may still load it | conditional note: read `AGENTS.md` *only if* its content is not already loaded; prefer `CLAUDE.md` if they appear to contradict |

Agentree cannot prevent a duplicate read; "do not read it twice" is an instruction to the lead, not an enforced property. A settings file alone never establishes loading, so every non-explicit result keeps a conditional pointer instead of silently relying on it. Explicit exclusions are never overridden: *dropped* and *excluded* add nothing to the prompt. `claude-md` only is treated as "not natively loaded", not as an exclusion, so it still gets the conditional request.

The settings read are `pluginConfigs[<id>].options.instructionFiles` and, for older configurations, `.options.projectInstructions`, where `<id>` is `cc-plugin-agents-md@builtin` or the older alias `agents-md@builtin` (the current id wins if both set an option), from the user `settings.json` (`CLAUDE_CONFIG_DIR` or `~/.claude`) and the organization's managed settings file, which overrides it (Claude Code reads plugin options from user, `--settings` and managed settings, not from project settings, so project files are not used for the mode). `--settings` cannot be seen, and whether `enabledPlugins` can switch the plugin off was not confirmed, so a configured mode stays *not verified*. `claudeMdExcludes` is read from the user, project (`.claude/settings.json`), local (`.claude/settings.local.json`) and managed settings files and merged; patterns are matched against absolute paths with `*`, `?` and `**` only. Patterns using `{}`, `[]`, `()` or `!`, or longer than 1024 characters, are not evaluated (see *excluded* above); at most 64 patterns and 8 KiB of pattern text are evaluated, and an exclusion is checked once per distinct imported file, counted against the import file limit. Matching is linear in pattern times path length, so a hostile pattern in a repository's settings cannot stall the launch. Matching is slightly broader than Claude Code's (case-folded on Windows, `**` crosses dot folders), which errs toward honouring an exclusion. These names come from inspecting Claude Code 2.1.x, not from a published contract, so they are treated as hints. A `CLAUDE.md` above the project (up to the drive root, excluding `~/.claude/CLAUDE.md`) is detected by existence only and never read.

Precedence between `AGENTS.md` and `CLAUDE.md` is **not enforced**. Agentree only asks, in the lead prompt, that `CLAUDE.md` be preferred when the two appear to contradict. Nested and user-level files stay Claude Code's own. `GEMINI.md`, `.github/copilot-instructions.md` and `.cursorrules` are listed for provenance and not used by the Claude backend.

## Import recognition

Approximates Claude Code's scan, which was read from the installed build rather than a specification: an `@path` that starts a line or follows whitespace (prose included), with `#fragment` ignored and `\ ` for spaces; `./`, `~/`, absolute and bare relative paths count. Not imports: anything in fenced or indented code, inline code spans, HTML comments, and `@` text glued to a word, quote, bracket or backslash (`"@AGENTS.md"`, `(@AGENTS.md)`, `\@AGENTS.md`). Code spans are removed together with HTML comments in one pass, so `` `<!--` `` is code, not a comment opener. Paths resolve relative to the file containing the import and are followed only inside the project, depth-first with one visited set as Claude Code does; `~/` and out-of-project imports are not followed. Claude Code refuses a file nested five levels down (`CLAUDE.md` is level 0), so `AGENTS.md` counts only as the **fourth** hop, although its documentation says "five hops". Edge cases differ (for example non-comment HTML blocks and emphasis splitting); an import that is missed shows up as *requested*, which is conditional and therefore safe.

## Limits

Each file is read up to 256 KiB; larger files are reported as such and not read. The import search follows at most 64 files, 1 MiB of imported text and 512 import paths. Exhausting a limit, or meeting an imported file that cannot be read as UTF-8 text, leaves the result *unverified* with a `Problem:` line; Agentree never reports an import, or its absence, that it did not establish.

## Content comparison

When both files are readable, separate and not imported, Agentree compares text only: line endings, trailing whitespace and outer blank lines are ignored. Results are *identical*, *same lines in a different order, repetition or spacing* (not treated as equivalent), or *differ textually* with counts of distinct lines in only one file. These are text differences, not a judgement that the instructions contradict; no semantic interpretation is attempted.

## Files that cannot be used

Each file is reported as absent, usable, outside the project, a broken link, not a regular file, unreadable, too large, or invalid UTF-8, with a reason but never its content. A file whose canonical path (symlinks, junctions and aliases resolved) is outside the canonical project folder is never opened and never pointed at; Claude Code itself may still load it. Unusable files are not compared and not reported as loaded.

Limits: settings files are read as found (a `.claude` junction pointing outside the project is not containment-checked for settings, only for instruction files); paths are resolved and then read, so a file swapped in between is not caught; hard links cannot be told apart from ordinary files by canonicalization; and symlink tests need a privilege Windows runners often lack, so they skip there while the junction test runs.

## Provenance

```powershell
agentree --instructions C:\path\to\project
```

Prints each file found, the settings seen, how `AGENTS.md` loads and any text difference. A missing or inaccessible project folder is an error with a nonzero exit status; file problems are printed as `Problem:` lines. At launch the status bar shows a line when attention is needed, and every launch writes the same text to `instructions-resolved.txt` in the run folder next to `instructions.txt`. If the project folder cannot be resolved at launch, the launch continues without a note and the reason is recorded there. See [`examples/instructions`](../examples/instructions) for a `CLAUDE.md` that imports `AGENTS.md`.

### Run manifest

The launch's decision is also recorded in `run-manifest.json` (see [Run provenance](PROVENANCE.md)). There, the `agents_md` values `import_syntax`, `native_configured` and `requested`, and the dispositions `identical_to_claude_md`, `import_syntax_detected`, `native_load_configured` and `lead_read_requested`, describe import syntax, settings, text identity or the prompt's request only. None is an observation that Claude Code loaded the file or that the lead read it. `claude_ingestion` and `lead_read` are always `not_observed`.
