# Workflow contracts

Agentree's role and workflow contracts (`src/workflow/`, Issue #9) describe how a team of agents is organised: which role does what, which model class it must use, which stages run on which path, what evidence passes between stages, and when review is mandatory. They are data plus validation. They start nothing.

## Scope

This module provides:

- a versioned definition format, `agentree.workflow/1`, with validation that reports every violation at once;
- automatic routing to a Fast, Balanced or Thorough path;
- a model registry and the two mandatory model policies (bulk reading and final review);
- evidence handoffs, source hashes, cache invalidation and bounded chunking;
- review packets, reviewer assignment, verdict validation and bounded repair.

It does not provide an execution engine (v0.8), a multi-provider runtime (v0.7) or a durable scheduler (v0.9). Nothing in this module calls Claude, starts a process or reads a model's output.

The consumer is the Claude adapter (`src/adapter.rs`, Issue #11). It takes a `ValidatedWorkflow`, calls `route::route` to choose a path, and uses `route::resolve` and `check_settings` to pick models and find conflicts. It is the only launch path that uses these contracts; see [Claude adapter](#claude-adapter).

## Definition format

A definition is JSON with `"schema": "agentree.workflow/1"` and `"version": 1`. A reader refuses a different schema and a newer version, and refuses unknown keys, so a misspelled setting such as `strict_enforcment` is an error and never a silent default. The registry, review packet, verdict and verification types are versioned together with this schema; the registry also carries its own `version`. The built-in definition is in [`examples/workflows/software-engineering.workflow.json`](../examples/workflows/software-engineering.workflow.json).

| Section | Fields |
| --- | --- |
| `roles[]` | `id`; `class` (`planning`, `bulk_read`, `implementation`, `verification`, `review`); `model` (`policy`: `lowest_cost`, `strongest_review`, `configured`, or `pinned` with `model`); `skill` and `mcp` (plugin references, see below); `tools`; `permission` (`read_only` or `edit`); `data_class` (default `repository`); `max_concurrent` (1 to 4) |
| `stages[]` | `id`; `role`; `depends_on` (always waited for); `optional_depends_on` (waited for only when that stage is on the chosen path); `min_path` (default `fast`); `inputs[]` (`name`, `from: {stage, output}` or `null` for values the run supplies, `required`); `outputs[]` (`name`, `kind`: `evidence`, `plan`, `diff`, `verification`, `verdict`, `report`); `owns` (workspace paths, implementation stages only; `.` is the whole workspace; `/` and `\` are equal, case is ignored, `.` components and a trailing separator are dropped); `policy` (`max_attempts` 1 to 3, `max_repair_cycles` 0 to 3, `on_exhausted`: `escalate` or `abort`); `inspection` (`max_requests` 1 to 8, `reader_role`) |
| `policy` | `allowed_capabilities` (default: all six tools); `max_path` (budget cap, default none); `strict_enforcement` (default `false`); `chunking` (`reserve_tokens`, `max_chunks`; `null` forbids chunking) |

The tool names are `read`, `search`, `shell`, `edit`, `web` and `mcp`.

**Plugin references.** A role's `skill` and `mcp` entries name a plugin and a skill or server by name only (`{"plugin": "software-engineering", "name": "explore"}`). The definition does not repeat anything the plugin declares. A reference resolves only through a *trusted* plugin (see [Agent Plugins](PLUGINS.md#trust)); trust never widens a role's tools or permission.

**Why it is separate.** Workflow metadata is kept out of the Agent Plugins v1.0 manifest. Plugin loading ignores unknown manifest fields and reports them as issues, so a `roles` or `workflow` block placed in `plugin.json` is noted and has no effect.

## Paths and routing

Three paths, in order: **Fast**, **Balanced**, **Thorough**. A stage runs on every path at or above its `min_path`. In the built-in workflow:

| Path | Stages |
| --- | --- |
| Fast | explore, implement, test, review |
| Balanced | Fast, plus research, plan, prepare-review |
| Thorough | Balanced, plus risk-scan |

`route::route` picks a path from a `TaskProfile` (complexity, risk, `needs_research`, `changes_code`):

1. Complexity sets the starting path: trivial and small start at Fast, medium at Balanced, large at Thorough.
2. Risk sets a floor: high raises to at least Balanced, critical to Thorough.
3. Research raises the path to at least Balanced.
4. A user `preferred` path raises it. It never lowers the result.
5. `policy.max_path` caps the result. The decision records `capped`.

The decision has no model field. **Routing never selects models**, and a higher-risk task gets more stages, not a different bulk reader or reviewer. `review_required` is set whenever the task changes code. The final review stage must be on every path, and validation enforces that.

## Model registry

`registry::Registry` lists `ModelEntry` values. Each has an `id`, `provider`, optional `aliases`, and these fields:

- `intelligence` and `cost` are separate explicit numbers. Intelligence is never derived from price, and cost is never derived from intelligence.
- `context_tokens`, `tools`, `max_permission`, `data_classes`: what the model can do.
- `available`: `false` when a discovery or health check found the model unavailable. Selection never assumes it. `true` means only "not known to be unavailable": the Claude adapter does not report such a model as a verified runtime model (see [Model policy](#model-policy)).

Reviewers are ranked by `intelligence`, always. `review_priority` only orders models of *equal* intelligence. A list that would put a weaker model ahead of, or leave out, a stronger one is a registry validation error, and selection ignores the conflict rather than obeying it. To keep a model from reviewing, restrict its `tools`, `max_permission` or `data_classes`: selection always honours those. `claude_default()` is the Claude Code registry (Haiku, Sonnet, Opus with aliases). Its numbers are defaults, and no selection code reads a model name.

### Bulk reading (`lowest_cost`)

Bulk-reading roles use the cheapest capable model. The cheapest tier is the lowest cost among *configured* capable models, whether or not they are available. Then:

1. If the whole input fits a model in that tier, use it.
2. If it does not fit but the policy allows chunking, split into windows of `context_tokens - reserve_tokens`, provided the chunk count is within `max_chunks`.
3. Otherwise, a more expensive model is never chosen silently. The result is an escalation:
   - the cheapest tier is configured but unavailable and nothing else is available: `RetryLater`;
   - a pricier model could run: `NeedsAuthorization`, listing the options;
   - nothing can run the work: `NoCapableModel`.

A pricier model runs only when a human passes an `Authorization` naming it. An authorization is never inferred.

### Final review (`strongest_review`)

The reviewer is the strongest eligible configured model (capable, large enough, highest `intelligence`). It is not necessarily the most expensive. Equally strong models are interchangeable, so an available one of the strongest tier is used with no authorization. If none is available, the stage is blocked: the result is `NeedsAuthorization`, listing weaker available models. Only an `Authorization` naming one of them allows that weaker reviewer, and the selection records `authorized_downgrade`. Authorizing the unavailable model changes nothing.

## Evidence handoffs

A bulk reader returns a `Handoff`: compact claims (1 to 400 characters each, at most 64 items), each pointing at workspace-relative source ranges and the SHA-256 of the whole file as it was read. A handoff is a pointer to evidence, not a copy of source. `Handoff::validate` rejects claims without references, absolute or `..` paths, malformed hashes and invalid line ranges.

`evidence::EvidenceCache` reuses evidence only while the file still hashes to the value it was read at. `plan` splits a file list into `reuse` and `scan`, reading each changed or new file once. `invalidate` drops entries whose file changed or was deleted. Nothing is served for text that has changed.

`evidence::chunk` packs files into windows of at most `window` tokens and splits a file larger than a window into parts. More windows than `max_chunks` is an error (`TooMany`), never a truncation.

## Review

1. **Packet.** `ReviewPacket` carries the objective, acceptance criteria, the actual diff (not a summary), verification results, optional evidence handoffs, and the list of implementers. `validate` rejects a packet without a diff, objective, criteria, verification or implementers.
2. **Independent reviewer.** `assign_reviewer` binds the selected model to a fresh agent. That agent must not be one that implemented the change. It may run the same model, because independence comes from a separate agent with no shared context.
3. **Inspection.** The reviewer may ask for up to `inspection.max_requests` more source ranges (`InspectionRequest`), answered by the bulk-reading role. Requests must stay inside the workspace and say what they need to confirm.
4. **Verdict.** `pass`, `needs_changes` or `blocked` (reported as the `PASS`, `NEEDS_CHANGES` and `BLOCKED` markers). `validate_verdict` accepts it only if:
   - it came from the assigned reviewer and model, and covers the packet's diff hash;
   - a `Pass` assesses correctness, security, regressions and unresolved risks, has no finding of medium severity or higher, no evidence gaps, and no failed, missing or contradictory verification (a `passed` result with a nonzero exit code is rejected in the packet and in the verdict);
   - `NEEDS_CHANGES` names a finding or an evidence gap;
   - `BLOCKED` says why review could not run.
5. **Bounded repair.** `next_action` decides what follows a failure. Model unavailable and policy violation always escalate. A transient failure retries while attempts remain. A failed verification or a `NEEDS_CHANGES` review starts a repair cycle while `max_repair_cycles` remains. Otherwise `on_exhausted` applies: `escalate` (the default) asks a human, `abort` stops. No loop ends by continuing silently.

## Validation

`WorkflowDef::validate` returns a `ValidatedWorkflow` only when it finds no violation. Violations are collected together, each with a code and a location, so one pass shows everything to fix. A dependency cycle or a schema error stops the later checks, because they depend on that structure. The checks:

- **Schema:** schema and version, name, id format, outputs declared, attempt and repair bounds, `max_concurrent` 1 to 4.
- **Duplicates and unknown references:** role and stage ids, output names, undefined roles, unknown dependencies, inputs from unknown stages or outputs, and inputs from stages that are not dependencies.
- **Cycles:** self-dependency and dependency cycles.
- **Capabilities:** unknown tools; tools the backend does not support; tools the workflow policy does not allow; only implementation roles may edit; a read-only role cannot hold `edit`; an implementation role needs `edit`; MCP servers need the `mcp` tool.
- **Model policies:** bulk-reading roles must use `lowest_cost`, and a capable model must exist; `strongest_review` is only for the reviewer, and a reviewer must exist; a pinned model must be in the registry and able to meet the role's requirements.
- **Plugins:** each referenced plugin is loaded, trusted and contains the named skill or server.
- **Ownership:** implementation stages own paths; other stages own none. Paths are normalized before comparison (`.`, `./`, `src`, `src/`, `src/./module.rs`, `SRC\Module.rs` and so on). A spelling that cannot be normalized reliably (absolute, drive-qualified, `..`, trailing dot or space, `~` short names, wildcards, control characters) fails validation and is treated as overlapping everything. Two writers that may run at the same time on any path cannot own overlapping paths.
- **Evidence:** bulk-reading stages produce evidence; implementation stages produce a diff.
- **Review:** a workflow that changes code has exactly one review stage, in its own role and independent of implementation. It runs on every path, every other stage is one of its dependencies, it takes required `diff`, `objective`, `acceptance_criteria` and `verification` inputs from the right stages, produces a verdict, allows inspection by a bulk-reading role, and depends on every implementation stage.
- **Per path:** each path has its own dependency graph, built only from the stages on it. A stage never waits for a stage omitted from the path: a `depends_on` stage must run on every path its dependant runs on, or the dependant must list it under `optional_depends_on`. A required input must come from a stage that runs there and is ordered before it. Every stage is a dependency of the review, and every implementation stage is verified, on each path. Ownership conflicts are judged on each path's graph. `ValidatedWorkflow::waits_for(path, stage)` returns the resolved dependencies for one path.
- **Enforcement:** see below. Violations appear here only when `strict_enforcement` is set.

## Enforcement

A policy is *enforced* only when the backend makes it impossible to break. Prompt instructions are *instruction only*. Agentree says which is which, and never presents an instruction as a technical gate.

`BackendProfile::claude_native()` describes Claude Code native teams today:

| Policy | Claude native teams | Why |
| --- | --- | --- |
| Lead model, effort, permission mode, advisor | Enforced | Launch flags (`--model`, `--effort`, `--permission-mode`, `--advisor`). Not checked by this module. |
| Teammate model (bulk readers on the lowest-cost model) | Instruction only | Written into the lead's prompt. Claude Code does not apply it to each teammate. |
| Teammate permission and tool lists | Instruction only | Same. |
| Final review gate | Observed, not prevented | The review marker `AGENTREE_REVIEW_RESULT` is observed after the fact. The lead can skip review. |
| Reviewer independence and diff delivery | Instruction only | On Claude the reviewer is the advisor, which sees the lead's whole conversation. It does not receive a separate `ReviewPacket`. |
| Lead session bulk reading | Instruction only | The lead is not a workflow role, so it can read bulk context itself on its own model. It is only asked to delegate. |
| Unnamed subagent model | Default set, instruction only | Claude has one default model for unnamed subagents. The adapter sets it through `CLAUDE_CODE_SUBAGENT_MODEL` to the cheapest of the bulk-reading roles' models, not the Coder's. Bulk roles that resolve to different models cannot all be matched by one variable: those roles must run as named teammates with their own model, and the lead is told so. A per-call `model` argument or an agent definition's model overrides the default, so the lead is also told to pass no override. `check_settings` reports a Coder model that is not the lowest-cost capable one, and the adapter drops that conflict only because it replaced the default. |

With `strict_enforcement: false` (the default), each gap is listed as a limitation on the `ValidatedWorkflow`. With `strict_enforcement: true`, each gap is a validation failure. The built-in workflow therefore fails strict validation on Claude native teams. That is the intended result until a backend can enforce these policies.

## Settings bridge

`route::check_settings` compares the saved role settings (`roles::Settings`, the Roles overlay) with the mandatory policies. It reports:

- a bulk-reading role whose configured model is not the lowest-cost capable model;
- a reviewer whose configured model is not the strongest eligible reviewer;
- the advisor set to `off`, which means the final review cannot run, or to a model that is not the strongest eligible reviewer (on Claude the review runs through the advisor);
- the Coder's model when it is not the lowest-cost capable model, because it is also the model of unnamed subagents.

A saved profile therefore cannot quietly override a mandatory policy. Today the built-in **Thorough** profile (Coder on Sonnet) and **Local** profile (advisor off) are reported; that is a real conflict with the contracts, shown rather than hidden. The check reports; it does not change settings.

## Claude adapter

`src/adapter.rs` (Issue #11) turns a `ValidatedWorkflow`, the saved role settings, the model registry (`registry::claude_default()`) and the trusted plugins into a launch. It runs for every Claude launch except `--demo`. The result is applied to the existing launch: one Claude lead and its named teammates. The adapter starts nothing and adds no scheduler.

It provides three things:

- a launch plan (`RolePlan`): the lead's flags, the unnamed-subagent environment variable and the teammate lines;
- the role section of the lead's instructions, which replaces the fixed role list. It begins with the stage flow: for each active stage, read from the validated workflow, what it waits for (`depends_on` plus the optional dependencies that run on the chosen path), which stages may run in parallel with it, its inputs (with their source stage and output, and whether required), its outputs, the workspace paths it owns, and its attempt and repair bounds with what happens on exhaustion. Like the rest of the section, this is an instruction: Claude Code does not schedule, gate or sandbox stages;
- `adapter-report.txt` in the run folder, beside `instructions.txt`: the enforcement table, saved-setting conflicts and notices.

If the workflow cannot be satisfied, the launch is blocked with every reason at once (`Launch blocked by workflow policy: ...`). Nothing is saved or spawned.

### What Claude Code enforces

| Policy | Level | How |
| --- | --- | --- |
| Lead model, effort, permission mode, advisor | Enforced | Launch flags `--model`, `--effort`, `--permission-mode`, `--advisor`. |
| Advisor model used for the final review | Enforced | `--advisor` is the alias of the strongest eligible reviewer (`opus`). That the review happens, and that it is independent, is not enforced (see below). |

### What it only instructs

| Policy | Level | Why |
| --- | --- | --- |
| Stage order, input and output contracts, path ownership, retry bounds | Instructed | Read from the validated workflow into the lead's prompt. Claude Code does not schedule, gate or sandbox stages. |
| Model runtime availability | Instructed | Selected models are configured and eligible; Agentree cannot verify that Claude Code can run them before launch. The lead is told to stop and ask, not substitute. |
| Teammate model, concurrency, permission mode and tools | Instructed | Written into the lead's prompt. Claude Code does not apply them to each named teammate. |
| Bulk reading goes to bulk-reading teammates | Instructed | The lead is asked to delegate and to take back compact evidence. It can still read on its own model. |
| Skills | Instructed | Offered only after the trust gate (below). The lead is given the path to each `SKILL.md`; Claude Code does not load plugin skills for named teammates. |
| Review packet and reviewer independence | Instructed | The advisor sees the lead's whole conversation, not a separate packet. |
| Final review cannot be skipped | Observed | The `AGENTREE_REVIEW_START` and `AGENTREE_REVIEW_RESULT` markers are read after the fact. |
| Git writes need human authorization | Instructed | Stated to the lead. Claude Code's own permission prompts still apply. |

### What is unsupported

MCP servers declared by trusted plugins are not passed to Claude Code, and the report lists them as unsupported. Agentree does not launch them, even when the plugin is trusted.

### Model policy

- **Bulk readers** (Explorer, Researcher) use the lowest-cost capable model in the registry. A saved pricier model is replaced and the replacement is reported. A pricier model is used only with an explicit human authorization. There is no interface for that yet, so the launch is blocked and lists the options.
- **Availability.** A model is *configured and eligible* when the registry lists it and selection accepts it. It is *verified* only if something confirmed that the running Claude Code can use it. Claude Code has no model-listing command and the capability probes do not start a session, so Agentree verifies nothing before launch (`Request::verified` is empty in `prepare`). The lead's instructions and `adapter-report.txt` list every selected model as "configured and eligible; runtime availability not verified" and tell the lead to stop and ask the human, not substitute a model, if Claude Code cannot run it. A registry entry marked unavailable still blocks the launch as before.
- **Final review** uses the strongest eligible model by `intelligence`, not the most expensive one, and the advisor must be able to run it. If the advisor cannot run it, or it is unavailable, the launch is blocked. A weaker reviewer needs an explicit human authorization. Nothing is downgraded silently.
- **Advisor off** stays off. The final review then ends BLOCKED, and the setting is reported as a conflict.
- **Other roles** use their saved model. A role with a `pinned` policy uses the workflow's model. Each teammate's concurrency is the lower of its saved setting and the workflow's `max_concurrent`.

### Routing and `--workflow-path`

`--workflow-path fast|balanced|thorough` sets the task complexity used by `route::route`: fast gives a small task (Fast path), balanced gives a medium one (Balanced path, the default) and thorough gives a large one (Thorough path). The adapter assumes medium risk and that code changes, so review is always required. The budget cap (`policy.max_path`) still applies, and routing never selects a model.

`--workflow FILE` runs an `agentree.workflow/1` definition instead of the built-in one. It is validated like the built-in one, with no fallback.

### Saved-setting conflicts

`route::check_settings` compares the saved settings with the mandatory policies (see [Settings bridge](#settings-bridge)). Each conflict is listed in `adapter-report.txt` and in the lead's instructions. The launch goes ahead with the policy applied; the adapter never changes a saved setting. For example, the Thorough profile's Coder on Sonnet no longer sets the unnamed-subagent default: the adapter replaces it with the cheapest bulk-reading model and reports that as a notice instead of a conflict. A pricier Explorer is both replaced and reported as a conflict. The advisor set to off is reported as described above.

### Skills and the trust gate

Only the bundled `software-engineering` plugin is loaded in this release. A `--workflow` file that names any other plugin is rejected as not loaded, even if that plugin is trusted.

The built-in workflow maps roles to the skills of the `software-engineering` plugin: Planner to `plan`, Explorer to `explore`, Researcher to `research`, Coder to `implement`, Tester to `test` and Final reviewer to `review`. The advisor uses `advise` from the review role's plugin. The Lead has no skill.

A skill is offered only if its plugin passes `Store::activate` at launch, which hashes the folder again. If the plugin is missing, untrusted or changed, the built-in workflow runs without skills and a notice gives the command to trust it, `agentree --plugin "<dir>" --plugin-trust`. A workflow file has no such fallback: validation fails.

The bundled plugin is searched for in this order: `AGENTREE_PLUGIN_DIR`; `%LOCALAPPDATA%\ClaudeAgentTree\plugins\software-engineering` (copied there by `install.ps1`); a `plugins\software-engineering` folder next to the executable; the same relative folder two levels up (for a build in a checkout's `target` folder); and the checkout's own `plugins` folder. If none is found, the notice says to re-run `install.ps1` or set `AGENTREE_PLUGIN_DIR`. Trust is per folder, so trust the copy that will be used.

Every run's `run-manifest.json` records the workflow name, the chosen path, the stage-to-role list in dependency order, and the skill-to-role mapping for that path, including the advisor. Skills in the mapping are recorded as offered, not as read. See [Run provenance](PROVENANCE.md).

### Strict mode

`strict_enforcement: true` turns each instruction-only or observed gap into a validation failure. On Claude native teams every strict workflow therefore fails validation, which is why the built-in workflow runs non-strict. With strict on, the launch also fails on any saved-setting conflict and on any unsupported policy, such as an MCP server.

## Not implemented yet

- **Review contract functions.** Nothing calls `assign_reviewer`, `validate_verdict` or `next_action` yet: on Claude the verdict arrives as a marker line, not a `ReviewVerdict`, and Agentree does not parse or validate it.
- **Named-teammate enforcement.** Claude Code does not apply a model, tool list or permission mode to a named teammate, so those remain instructions (see [Claude adapter](#claude-adapter)).
- **Concurrency** is the per-role `max_concurrent` (1 to 4); there is no workflow-wide parallel cap.
- **Spend limits** and **durable repair loops** (state that survives a restart) are v0.9.
- **Pi, local models and other backends** are v0.7. `BackendProfile` is the place to describe each one.
- The Roles overlay's teammate settings remain instructions. Agentree cannot confirm them in a live session.
