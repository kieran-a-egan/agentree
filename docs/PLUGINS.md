# Agent Plugins

Agentree loads [Agent Plugins v1.0](https://agent-plugins.org) (schema `1.0.0`): a folder holding a `plugin.json`, optional skills and optional MCP server declarations. Loading only reads and validates files. Nothing in a plugin runs unless you trust it, and trust is bound to the plugin's exact contents.

The loader is `src/plugins.rs`. Role and workflow definitions that *use* plugin skills are Agentree's own and live outside this standard (see [Workflow contracts](WORKFLOWS.md)).

## Layout

```text
my-plugin/
  plugin.json            required
  mcp.json               optional: MCP servers
  skills/
    code-review/
      SKILL.md           a folder with an exact SKILL.md is a skill
      references/        files here are listed as the skill's references
```

A working example is in [`examples/plugins/code-review`](../examples/plugins/code-review).

## What is validated

**`plugin.json`** must be a JSON object with `$schema` (exactly the 1.0.0 plugin schema URL) and `name` (1-64 characters of `a-z 0-9 - .`, starting and ending with a letter or digit, no `--` or `..`). Metadata (`version`, `description`, `homepage`, `repository`, `license`, `keywords`, `author`, `extensions`) is checked by JSON type only; `author` may hold only `name`, `email` and `url`. A bad manifest rejects the whole plugin: no skill or MCP server from it is listed or run. An unknown top-level field, or an `extensions` that is not an object, is reported and ignored.

**Skills** are the immediate sub-folders of `skills/` that contain `SKILL.md`; nothing deeper is searched. A skill needs frontmatter `name` equal to its folder name (lower-case letters, digits and `-`) and a `description` of 1-1024 characters. A skill that fails is skipped and reported; others still load. References are the regular files directly inside the skill's `references/`. That folder is resolved before it is listed: a symlink or junction leading outside the plugin is reported and its contents are neither listed nor named.

**`mcp.json`** must hold exactly `$schema` (the 1.0.0 MCP schema URL) and `mcpServers`. Each server must match one closed variant:

| `type` | Fields | Rules |
| --- | --- | --- |
| `stdio` | `command`, `args`, `env`, `cwd` | `command` is one executable name, or a `./` path inside the plugin. `args`, `env` values and `cwd` expand `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` once. `env` may not set those two names. `cwd` must be `./...`, or start with `${PLUGIN_ROOT}` or `${PLUGIN_DATA}`, and stay inside that folder. |
| `streamable-http`, `sse` | `url`, `headers` | `url` is absolute `http(s)`, with no user info or fragment, and `https` unless the host is `localhost` or a loopback address. Header names are valid tokens and not repeated in another case. |

A server that fails is skipped and reported; a bad `mcp.json` disables MCP for that plugin only. Plugins must not put credentials in `env` or `headers`; Agentree does not scan for them.

**Containment.** Every skill, reference, bundled command and working directory is resolved through symlinks and junctions and must land inside the plugin folder; one that does not is skipped as above. Containment covers files only. It does not sandbox a plugin's subprocess.

Everything skipped or ignored is listed as a *note* by `--plugin`.

## Built-in software-engineering plugin

[`plugins/software-engineering`](../plugins/software-engineering) lives in this repository: seven skills, no MCP servers. Each is a self-contained folder, so a role can use one without the others, and each `SKILL.md` documents its **Inputs**, **Procedure**, **Outputs** and **Limits**.

| Role | Skill | Takes | Produces | Edits files |
| --- | --- | --- | --- | --- |
| Planner | `plan` | objective, context | ordered tasks with owners, files and done-checks | no |
| Explorer | `explore` | question, scope | answer with `file:line` evidence | no |
| Researcher | `research` | topic, decision, constraints | sourced findings and confidence | no |
| Coder | `implement` | task, owned files | changes and verification | owned files only |
| Tester | `test` | change, commands | result, commands run, tests added | test files only |
| Advisor | `advise` | question, evidence | one recommendation with reasoning | no |
| Final reviewer | `review` | diff, objective, evidence | `PASS` / `NEEDS_CHANGES` / `BLOCKED` with findings | no |

`roles::Role::skill()` is a fixed role-to-skill table; the Lead has none. The Claude adapter (`src/adapter.rs`) maps the built-in workflow's roles to these skills. A skill is offered to a role only if its plugin passes `Store::activate` at launch, and the lead is given the path to its `SKILL.md`. Claude Code does not load plugin skills for named teammates, so this is an instruction, not a guarantee. The advisor and final reviewer run through Claude's parameterless advisor tool, which cannot load a skill itself, so the lead is told to read the `review` skill before the final review and the `advise` skill before it consults the advisor for a blocker. Tests check that the plugin loads with no issues, that every role resolves to exactly one skill, and that each `SKILL.md` has Inputs, Outputs and Limits sections.

The plugin is trusted like any other (`agentree --plugin plugins\software-engineering --plugin-trust`). `install.ps1` copies it to `%LOCALAPPDATA%\ClaudeAgentTree\plugins\software-engineering`. Trust is bound to that folder, so review and trust the copy with `agentree --plugin "%LOCALAPPDATA%\ClaudeAgentTree\plugins\software-engineering" --plugin-trust`. An untrusted or changed plugin is not an error: the built-in workflow runs without skills and the launch report says how to trust it.

## Trust

A plugin starts untrusted. Untrusted, or changed since it was trusted, it activates nothing: `plugins::Store::activate` returns no skills and no MCP servers, and the launch path must obtain them only through it. A repository can therefore ship a plugin but cannot turn it on.

```powershell
agentree --plugin C:\path\to\my-plugin                  # review: skills, the exact MCP commands, notes
agentree --plugin C:\path\to\my-plugin --plugin-trust   # trust it as it is now
agentree --plugin C:\path\to\my-plugin --plugin-revoke  # withdraw trust
```

Review the report before trusting: for each MCP server it shows the command and arguments that would run. Trust is recorded in `%LOCALAPPDATA%\ClaudeAgentTree\plugins.json` (never in the repository) against the plugin's folder and a SHA-256 over the path and contents of every file in it, `.git` included, and the target of every link. Editing any file, including a bundled script, makes the plugin *changed* and untrusted until you trust it again. Retargeting a link counts as a change. Because `.git` is hashed, a git operation inside a trusted plugin also makes it *changed*; that fails safe. A copy of a trusted plugin in another folder is untrusted. A `plugins.json` that cannot be read or comes from a newer Agentree is left untouched and trusts nothing.

Trust covers the plugin's own files. `args` and `env` values are opaque strings: a file they name outside the plugin is not hashed or contained, and `env` such as `PATH` or `NODE_OPTIONS` can change what runs, so the report shows every `env` entry and the working directory next to the command. Read them before trusting.

Trust is re-checked against the disk, not against the loaded object. `trust` and `activate` hash the folder again and fail closed: a file that changed, vanished or cannot be read, or a plugin folder that was moved or relinked since it was loaded, makes `trust` refuse and `activate` return nothing until the plugin is loaded, reviewed and trusted again. Loading itself hashes before and after parsing and rejects a plugin whose contents differ after parsing from before; a change made and reverted between those two hashes is not detected.

**Limits.** This is a check at a moment in time, not a lock. A file can still change between the final hash and the moment a launcher starts a process from it, and a plugin that is rewritten during a hash may be read file by file, so a hash is not a snapshot. Agentree does not claim atomicity here; the launch path should call `activate` immediately before starting anything, and a plugin folder that other processes can write to is not safe to trust. `--plugin-trust` loads the folder again and trusts what is on disk at that moment, so a change since an earlier review run is not caught: compare the report it prints with what you reviewed, and `--plugin-revoke` if it differs.

A plugin may contain at most 10,000 entries and 64 MiB; larger ones are refused.

## Provenance

A run that offers plugin skills records them in `run-manifest.json` (see [Run provenance](PROVENANCE.md)). A plugin appears as `activated`: it was trusted at that fingerprint when the brief was built. A skill appears as `offered`: its path was written into the lead's instructions. Whether anyone read a skill is never observed (`observed: not_observed`), and whether a named teammate used its skill is unverified. MCP servers appear only as `declared_not_launched`.

## Not in this version

- Plugin MCP servers are validated and can be trusted, but no launch runs them. The Claude adapter does not pass them to Claude Code, and the adapter report lists them as unsupported. Use `--plugin` to inspect and trust a plugin; trust does not start its servers.
- `PLUGIN_DATA` folders (`%LOCALAPPDATA%\ClaudeAgentTree\plugin-data\<name>`) are computed but created only when a server is launched.
- Skill frontmatter is checked for the fields the Agent Skills specification requires, not for every optional field.
