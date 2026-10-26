# Run provenance

Every Claude-native run writes `run-manifest.json` into its run folder, next to `instructions.txt`, `instructions-resolved.txt` and `adapter-report.txt` (which are unchanged). The manifest records which instruction files were found and hashed, the resolver's decision, which trusted plugin and role skills were offered to the lead, and how far the launch got. It describes what Agentree decided and offered. It does not show what Claude Code loaded or did. Source: `src/provenance.rs`.

## Location and format

- Path: `%LOCALAPPDATA%\ClaudeAgentTree\runs\<run-id>\run-manifest.json`.
- Schema `agentree.run-manifest`, version `1`. Readers refuse any other schema or version.
- Pretty JSON, two-space indent, fixed field order, trailing newline. The same decisions always produce the same bytes.
- Written through `run-manifest.json.tmp` and renamed into place, so a crash never leaves half a file.
- Reads refuse files over 256 KiB.

## Inspect and export

```powershell
agentree --run-manifest 6f1c2a9e-3b7d-4e58-9a10-c2d4e6f8a0b1
```

Prints the sanitized manifest to stdout. The command is read-only. The run id is the session id shown in the status bar, which is also the run folder name. It must be a lowercase hyphenated UUID.

The export is re-rendered from the typed model after validation, never copied from the file. Agentree refuses to read when:

- the run folder or the manifest is a link, a junction or not a plain file, or the real path is outside the runs folder;
- the manifest is missing (a run from before this feature, or a write that failed);
- the file is larger than 256 KiB;
- the file is not JSON, is truncated, or does not match the schema (`corrupt`);
- the schema or version is not supported by this Agentree (`unsupported`);
- the file is valid but breaks a rule in `provenance::validate` (`invalid`);
- the file cannot be read (`io`).

Each error prints a short message and the command exits with an error. Messages never repeat file content.

The status file `startup-status.json` gains `run_manifest_present`, which is true when the run folder holds a manifest.

## Field reference

| Field | Meaning |
| --- | --- |
| `schema`, `version` | Always `agentree.run-manifest` and `1`. |
| `run_id` | Session id, lowercase hyphenated UUID. Equals the run folder name. |
| `agentree_version` | Crate version that wrote the file. |
| `backend` | `claude_native_teams`, the only backend. |
| `launch` | How far the launch got. See [Launch states](#launch-states). |
| `workflow` | `null` for a simulated run. Otherwise `name`, `path` (`fast`, `balanced`, `thorough`) and `stages`, a list of `{stage, role}` in dependency order for the chosen path. |
| `lead_policy` | `artifact` (always `instructions.txt`), `sha256` and `bytes` of the policy text, and `instruction_note`: `null`, or `{kind, sha256}` for the project-instruction line Agentree appended. |
| `instructions` | The resolver's decision: `resolution`, `settings`, `agents_md`, `text_comparison`, `import_search_complete`, `claude_file_above_project`, `claude_ingestion` and `lead_read` (both always `not_observed`), and `sources`. |
| `instructions.sources[]` | One entry for `AGENTS.md` and each `CLAUDE.md`-family path (absent ones included), plus each other-tool file that exists; sorted by `path`: `path` (project-relative), `family`, `file`, `bytes` and `sha256` (present only for `ok` and `invalid_utf8` files of the `shared` and `claude` families; other tools' files are located but never read, so they carry no digest), `excluded` and `disposition`. |
| `plugins[]` | Plugins that passed `Store::activate` for something offered to this run: `name`, `version`, `fingerprint` (SHA-256 over the plugin's files) and `status`. |
| `skills[]` | Skills offered to this run: `role`, `plugin`, `skill`, `version`, `fingerprint`, `source` (plugin-relative `SKILL.md` path), `audience`, `status` and `observed` (always `not_observed`). |
| `mcp[]` | MCP servers the trusted plugins declare for a role: `role`, `plugin`, `server` and `status`. None is ever launched. |
| `unavailable[]` | Skills or MCP servers the workflow named that were not offered: `role`, `kind`, `plugin`, `name` and `reason`. |
| `limitations[]` | The fixed list of 12 limitation codes. See [Limitation codes](#limitation-codes). |
| `warnings[]` | `code` and an optional project-relative `subject`. See [Warning codes](#warning-codes). |

### Launch states

| Value | Meaning |
| --- | --- |
| `prepared` | Written before the Claude process starts. The start is not recorded yet. |
| `spawned` | The operating system accepted the process start. Nothing after that is observed. |
| `spawn_failed` | Starting the process failed. No instruction or skill reached a Claude session. |
| `simulated` | Demo mode: no Claude process, no workflow, no skills. |

### Enumerations

| Field | Values |
| --- | --- |
| `instructions.resolution` | `resolved`; `failed` (the resolver could not run, for example the project folder was unusable). |
| `instructions.settings` | `unset` (no setting found); `claude_only`; `agents_fallback`; `both`; `dropped` (settings drop project instruction files); `unusable` (user settings could not be read). |
| `instructions.agents_md` | `absent`; `same_file`; `import_syntax` (a `CLAUDE.md` import reaches it); `native_configured` (your settings make Claude Code load it, not verified); `dropped`; `excluded`; `requested` (the lead is asked to read it unless already loaded); `unverified`. |
| `instructions.text_comparison` | `identical`; `same_lines` (same lines in another order, repetition or spacing); `differs`. Absent when the files are not both readable and separate. |
| `sources[].family` | `shared` (`AGENTS.md`); `claude` (`CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`); `other` (`GEMINI.md`, `.github/copilot-instructions.md`, `.cursorrules`). |
| `sources[].file` | `absent`; `ok`; `outside_project` (real location outside the project, never read); `broken_link`; `not_a_file`; `unreadable`; `invalid_utf8`; `too_large` (over 256 KiB). |
| `sources[].disposition` | `absent`; `unusable` (found but cannot be used); `not_used_by_backend` (other tools' files); `claude_native_expected` (Claude Code is expected to load it; not observed); `dropped`; `excluded`; `same_file_as_claude_md`; `identical_to_claude_md` (same text as a live `CLAUDE.md`; no request added); `import_syntax_detected` (import syntax only); `native_load_configured` (settings say so; not confirmed); `lead_read_requested` (the lead is asked to read it unless already loaded); `unverified`. |
| `instruction_note.kind` | `read_requested_unless_loaded`; `conditional_pointer`. |
| `plugins[].status` | `activated`: trusted at this fingerprint and activated just before the brief. |
| `skills[].status` | `offered`: its path and plugin identity are in the lead's instructions. This is not proof of use. |
| `skills[].audience` | `lead`: the lead is told to read the file. `teammate_prompt`: the lead is told to put "read this file" into a named teammate's prompt. |
| `mcp[].status` | `declared_not_launched`: the plugin declares it; Agentree starts no MCP server and cannot hand it to Claude Code in this release. |
| `unavailable[].kind` | `skill`; `mcp`. |
| `unavailable[].reason` | `not_loaded` (missing, or rejected when loaded); `ambiguous` (two loaded plugins share the name); `untrusted` (never trusted, or revoked); `changed` (trusted before, files changed or unreadable now); `no_such_item` (trusted plugin has no such skill or server); `suppressed_by_fallback` (the skill is available from a trusted plugin, but the built-in workflow fell back to its skill-free form because another requested skill was missing, untrusted or changed, so this one is not offered; it is not missing or untrusted). Every skill the built-in workflow requested for a role it runs appears either in `skills` or in `unavailable`. |

### Warning codes

| Code | Meaning |
| --- | --- |
| `instructions_unresolved` | The resolver failed; `instructions` has no sources. |
| `source_unusable` | A candidate file is not absent and not usable. `subject` names it. |
| `source_omitted` | `AGENTS.md` is usable, but settings drop or exclude it, so no request is added. |
| `import_search_incomplete` | The `@import` search hit a read limit or an unreadable file. |
| `exclusions_unchecked` | Some `claudeMdExcludes` pattern could not be evaluated. |
| `settings_unusable` | The user or managed settings could not be read. |
| `claude_file_above_project` | A `CLAUDE.md`-family file exists above the project. Existence only. |
| `skill_unavailable` | A workflow skill was not offered. See `unavailable`. |
| `mcp_unavailable` | A workflow MCP server was not offered. See `unavailable`. |

### Limitation codes

Each code is always present, in this order. [Proven vs unverified](#proven-vs-unverified) explains what each one covers.

| Code | Meaning |
| --- | --- |
| `claude_ingestion_not_observed` | Whether Claude Code loaded any instruction file is not observed. |
| `import_ingestion_not_observed` | Whether Claude Code followed an `@import` is not observed. |
| `lead_read_not_confirmed` | Whether the lead did the conditional read it was asked to do is not known. |
| `skill_read_not_confirmed` | Whether anyone read a `SKILL.md` is not known. |
| `teammate_skill_use_unverified` | Whether a named teammate received or used its skill text is not known. |
| `mcp_servers_not_launched` | No MCP server was started or handed to Claude Code. |
| `command_line_settings_not_inspected` | `--settings` values were not read. |
| `nested_instructions_not_inspected` | Instruction files in subfolders were not read. |
| `user_instructions_not_inspected` | User-level instruction files were not read. |
| `managed_instructions_not_inspected` | Managed instruction files were not read (managed settings are read for the mode only). |
| `files_may_change_after_inspection` | Files were hashed when resolved; they may change before Claude reads them. |
| `plugin_may_change_after_activation` | A plugin's fingerprint is as trusted at brief time; activation is not repeated at spawn. |

## Sample

A realistic manifest for a balanced run with an `AGENTS.md` beside a differing `CLAUDE.md`. All digests are placeholders.

```json
{
  "schema": "agentree.run-manifest",
  "version": 1,
  "run_id": "6f1c2a9e-3b7d-4e58-9a10-c2d4e6f8a0b1",
  "agentree_version": "0.4.4",
  "backend": "claude_native_teams",
  "launch": "spawned",
  "workflow": {
    "name": "software-engineering",
    "path": "balanced",
    "stages": [
      { "stage": "research", "role": "researcher" },
      { "stage": "explore", "role": "explorer" },
      { "stage": "plan", "role": "planner" },
      { "stage": "implement", "role": "coder" },
      { "stage": "test", "role": "tester" },
      { "stage": "prepare-review", "role": "explorer" },
      { "stage": "review", "role": "final_reviewer" }
    ]
  },
  "lead_policy": {
    "artifact": "instructions.txt",
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "bytes": 4312,
    "instruction_note": {
      "kind": "read_requested_unless_loaded",
      "sha256": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
    }
  },
  "instructions": {
    "resolution": "resolved",
    "settings": "unset",
    "agents_md": "requested",
    "text_comparison": "differs",
    "import_search_complete": true,
    "claude_file_above_project": false,
    "claude_ingestion": "not_observed",
    "lead_read": "not_observed",
    "sources": [
      {
        "path": ".claude/CLAUDE.md",
        "family": "claude",
        "file": "absent",
        "bytes": null,
        "sha256": null,
        "excluded": false,
        "disposition": "absent"
      },
      {
        "path": "AGENTS.md",
        "family": "shared",
        "file": "ok",
        "bytes": 1204,
        "sha256": "1111111111111111111111111111111111111111111111111111111111111111",
        "excluded": false,
        "disposition": "lead_read_requested"
      },
      {
        "path": "CLAUDE.local.md",
        "family": "claude",
        "file": "absent",
        "bytes": null,
        "sha256": null,
        "excluded": false,
        "disposition": "absent"
      },
      {
        "path": "CLAUDE.md",
        "family": "claude",
        "file": "ok",
        "bytes": 2317,
        "sha256": "2222222222222222222222222222222222222222222222222222222222222222",
        "excluded": false,
        "disposition": "claude_native_expected"
      }
    ]
  },
  "plugins": [
    {
      "name": "software-engineering",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "status": "activated"
    }
  ],
  "skills": [
    {
      "role": "advisor",
      "plugin": "software-engineering",
      "skill": "advise",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/advise/SKILL.md",
      "audience": "lead",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "coder",
      "plugin": "software-engineering",
      "skill": "implement",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/implement/SKILL.md",
      "audience": "teammate_prompt",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "explorer",
      "plugin": "software-engineering",
      "skill": "explore",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/explore/SKILL.md",
      "audience": "teammate_prompt",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "final_reviewer",
      "plugin": "software-engineering",
      "skill": "review",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/review/SKILL.md",
      "audience": "lead",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "planner",
      "plugin": "software-engineering",
      "skill": "plan",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/plan/SKILL.md",
      "audience": "teammate_prompt",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "researcher",
      "plugin": "software-engineering",
      "skill": "research",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/research/SKILL.md",
      "audience": "teammate_prompt",
      "status": "offered",
      "observed": "not_observed"
    },
    {
      "role": "tester",
      "plugin": "software-engineering",
      "skill": "test",
      "version": "1.0.0",
      "fingerprint": "4444444444444444444444444444444444444444444444444444444444444444",
      "source": "skills/test/SKILL.md",
      "audience": "teammate_prompt",
      "status": "offered",
      "observed": "not_observed"
    }
  ],
  "mcp": [],
  "unavailable": [],
  "limitations": [
    "claude_ingestion_not_observed",
    "import_ingestion_not_observed",
    "lead_read_not_confirmed",
    "skill_read_not_confirmed",
    "teammate_skill_use_unverified",
    "mcp_servers_not_launched",
    "command_line_settings_not_inspected",
    "nested_instructions_not_inspected",
    "user_instructions_not_inspected",
    "managed_instructions_not_inspected",
    "files_may_change_after_inspection",
    "plugin_may_change_after_activation"
  ],
  "warnings": []
}
```

## Proven vs unverified

Proven means Agentree established it from its own reads and decisions at the time of the run. Not proven means the manifest never claims it. Those items are always `not_observed` or listed in `limitations`.

| Claim | Status | Basis |
| --- | --- | --- |
| Which candidate instruction files were found, and their state | Proven at resolve time | `sources[].file` |
| SHA-256 and length of the bytes Agentree read | Proven for those bytes | `sources[].sha256`, `bytes` |
| The resolver's decision (settings mode, `AGENTS.md` delivery, disposition) | Proven as Agentree decided it, from the files and settings it read | `instructions` |
| The plugin passed `Store::activate` at this fingerprint when the brief was built | Proven at brief time | `plugins[].status` |
| The skill's path and plugin identity were written into the lead's instructions | Proven as text in the policy | `skills[].status: offered`; `lead_policy.sha256` matches `instructions.txt` |
| The process start | Proven only that the OS accepted it | `launch` |
| Claude Code loaded `CLAUDE.md`, `AGENTS.md` or any import | **Not proven** | `claude_ingestion: not_observed` |
| The lead performed the conditional read it was asked for | **Not proven** | `lead_read: not_observed` |
| Anyone read a `SKILL.md` | **Not proven** | `skills[].observed: not_observed` |
| A named teammate received its skill text | **Not proven** | `teammate_skill_use_unverified` |
| MCP servers ran or were available to Claude Code | **Not proven**; none is launched | `mcp[].status: declared_not_launched` |
| Command-line `--settings` | **Not inspected** | `command_line_settings_not_inspected` |
| Nested, user-level or managed instruction files | **Not inspected** | `nested_instructions_not_inspected`, `user_instructions_not_inspected`, `managed_instructions_not_inspected` |
| Ancestor `CLAUDE.md` files | Existence only, never read | `claude_file_above_project` |

## Privacy and export rules

- The manifest is a closed, typed model. Every field is an enum, an identifier, a count, a path relative to its root, or a SHA-256 digest. Unknown fields are rejected on read.
- It never contains: credentials; instruction or skill contents; prompts; objectives; transcripts; MCP commands, arguments, environment or config; absolute paths; control characters; free-form error text.
- Free-form values are not copied. A workflow name is exported as written only if it is the built-in `software-engineering`; any other name becomes `sha256-` and 12 hex digits of its SHA-256. A plugin version is exported as written only if it is `N`, `N.N`, `N.N.N` or `N.N.N.N` (up to nine digits each), optionally followed by `-alpha`, `-beta`, `-rc`, `-pre` or `-dev` and an optional `.N`; anything else (including `1.0.0-<anything else>` and build metadata) becomes the same `sha256-` form. The form identifies a value for comparison; it does not contain it.
- Other identifiers (plugin, skill, role, stage and MCP server names, and each component of a path) are exported as written only if they are at most 96 characters of `A-Z a-z 0-9 . _ -` with no run of letters and digits longer than 24 characters; otherwise they become the `sha256-` form. This is a shape check, not secret detection. Plugin and skill names are already limited by the plugin loader to lowercase letters, digits, `-` and `.` (at most 64 characters); role and stage ids to letters, digits, `-` and `_` (at most 48). MCP server names are the keys of the plugin's `mcp.json` and are not otherwise limited, so only the shape check applies to them. A short secret that passes the shape check, such as a 20-character access key id, would still be exported as a name. The same checks run on read, so a hand-edited manifest carrying other text is refused.
- Paths are project-relative for instruction files and plugin-relative for skills.
- Errors from `--run-manifest` are fixed, categorical messages (not valid JSON, truncated, schema mismatch, unrecognized schema, a version number, or a named rule). They never quote the file's schema string, field names, values or parser text.
- Hashes are the only trace of file content, and the `sha256-` forms are hashes of short values. A SHA-256 of a low-entropy input (a common `CLAUDE.md` template, or a guessable workflow name) can be matched by anyone who already has or can guess the same input. The export is built to leave out credentials and file contents, but Agentree cannot prove that an export is safe to publish; review it before sharing outside your team. It is intended for internal sharing and audit.

## Failure policy

1. Agentree resolves the instructions once. The same result feeds `instructions-resolved.txt`, the lead's note and the manifest.
2. The manifest is built from that resolution and the same adapter brief used for the launch. It is written after `instructions.txt`, `instructions-resolved.txt` and `adapter-report.txt`, and before the Claude process starts, as `prepared` (or `simulated` in `--demo`, with no workflow and no skills).
3. After the process start, it is rewritten as `spawned` or `spawn_failed`.
4. A write failure does not block the launch. The provenance manifest records no permission or trust decision. The status bar shows `Provenance manifest not recorded: <reason>`. The temporary file is removed and any earlier file is left as it was.
5. A launch that is blocked (an invalid workflow or policy, or a spawn precondition that fails before the run folder exists) writes no manifest.
6. If the final rewrite fails after a successful start, the file still says `prepared` and the status bar shows the error. Read `prepared` as "not known to have started".

## Limitations

- **Filesystem races.** Files are hashed when resolved; Claude reads them later. A file swapped in between is not caught.
- **Plugins after activation.** The fingerprint is the one trusted at brief time. `Store::activate` is not run again at spawn, so a change in that window is not caught.
- **Older runs.** Runs from before this feature have no manifest.
- **Read limits.** Instruction files over 256 KiB, and import searches over their limits, are reported as unverified. See [Project instructions](INSTRUCTIONS.md#limits).
- **Schema version.** `unavailable[].reason` gained `suppressed_by_fallback` without a version bump. A build from before that value rejects a manifest that uses it as corrupt. Reading is also stricter than before: a manifest whose names or versions contain text the exporter would now replace (a space, `+`, a long unbroken run, a free-form version) is refused as invalid.
- **Advisor skill after a fallback.** The advisor skill is resolved separately from the role skills. When the built-in workflow falls back because a role skill is missing, the advisor skill can still be offered, so `skills` and `plugins` may hold that one entry beside a list of `unavailable` role skills.
- **Not tamper-evident.** There is no signature. Anyone who can write the run folder can change the file, and `--run-manifest` checks its shape and rules, not its authenticity.

## Relationship to the v0.6 release gate

The v0.6 gate (Issue #35, see [ROADMAP.md](../ROADMAP.md)) asks that different roles load the expected skills, that invalid or untrusted plugins do not execute, and that instruction files, plugins and skill versions used in each run are tracked. The manifest is evidence for those checks. For the last point it records what was inspected, activated and offered, with digests and versions. It does not show that any skill was read or that any instruction was followed. Gate reviewers should read `limitations` and every `not_observed` field with the evidence.
