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

The consumer is the Claude adapter (Issue #11). It takes a `ValidatedWorkflow`, calls `route::route` to choose a path, and uses `route::resolve` and `Registry::select_*` to pick models. Until #11 lands, no launch path uses these contracts, and the default Claude Code launch is unchanged.

## Definition format

A definition is JSON with `"schema": "agentree.workflow/1"` and `"version": 1`. A reader refuses a different schema and a newer version, and refuses unknown keys, so a misspelled setting such as `strict_enforcment` is an error and never a silent default. The registry, review packet, verdict and verification types are versioned together with this schema; the registry also carries its own `version`. The built-in definition is in [`examples/workflows/software-engineering.workflow.json`](../examples/workflows/software-engineering.workflow.json).

| Section | Fields |
| --- | --- |
| `roles[]` | `id`; `class` (`planning`, `bulk_read`, `implementation`, `verification`, `review`); `model` (`policy`: `lowest_cost`, `strongest_review`, `configured`, or `pinned` with `model`); `skill` and `mcp` (plugin references, see below); `tools`; `permission` (`read_only` or `edit`); `data_class` (default `repository`); `max_concurrent` (1 to 4) |
| `stages[]` | `id`; `role`; `depends_on`; `min_path` (default `fast`); `inputs[]` (`name`, `from: {stage, output}` or `null` for values the run supplies, `required`); `outputs[]` (`name`, `kind`: `evidence`, `plan`, `diff`, `verification`, `verdict`, `report`); `owns` (workspace paths, implementation stages only; `.` is the whole workspace); `policy` (`max_attempts` 1 to 3, `max_repair_cycles` 0 to 3, `on_exhausted`: `escalate` or `abort`); `inspection` (`max_requests` 1 to 8, `reader_role`) |
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
- `available`: runtime availability, set from discovery or health checks. Selection never assumes it.

`review_priority` is an explicit order, strongest first. When it is not empty it is also an allowlist: a model not listed is never a reviewer. When it is empty, `intelligence` ranks reviewers. `claude_default()` is the Claude Code registry (Haiku, Sonnet, Opus with aliases). Its numbers are defaults, and no selection code reads a model name.

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

The reviewer is the strongest eligible configured model (capable, large enough, first in review order). It is not necessarily the most expensive. If it is unavailable, the stage is blocked: the result is `NeedsAuthorization`, listing weaker available models. Only an `Authorization` naming one of them allows that weaker reviewer, and the selection records `authorized_downgrade`. Authorizing the unavailable model changes nothing.

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
   - a `Pass` assesses correctness, security, regressions and unresolved risks, has no finding of medium severity or higher, no evidence gaps, and no failed or missing verification;
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
- **Ownership:** implementation stages own paths; other stages own none; paths are relative and inside the workspace; two writers that may run at the same time cannot own overlapping paths.
- **Evidence:** bulk-reading stages produce evidence; implementation stages produce a diff.
- **Review:** a workflow that changes code has exactly one review stage, in its own role and independent of implementation. It runs on every path, every other stage is one of its dependencies, it takes required `diff`, `objective`, `acceptance_criteria` and `verification` inputs from the right stages, produces a verdict, allows inspection by a bulk-reading role, and depends on every implementation stage.
- **Per path:** on each path, a required input must come from a stage that also runs there, and every implementation stage must be verified by a verification stage on that path.
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
| Unnamed subagent model | Flagged by `check_settings` | Claude has one model for unnamed subagents and Agentree sets it to the Coder's. A pricier Coder moves exploration subagents off the lowest-cost tier. |

With `strict_enforcement: false` (the default), each gap is listed as a limitation on the `ValidatedWorkflow`. With `strict_enforcement: true`, each gap is a validation failure. The built-in workflow therefore fails strict validation on Claude native teams. That is the intended result until a backend can enforce these policies.

## Settings bridge

`route::check_settings` compares the saved role settings (`roles::Settings`, the Roles overlay) with the mandatory policies. It reports:

- a bulk-reading role whose configured model is not the lowest-cost capable model;
- a reviewer whose configured model is not the strongest eligible reviewer;
- the advisor set to `off`, which means the final review cannot run, or to a model that is not the strongest eligible reviewer (on Claude the review runs through the advisor);
- the Coder's model when it is not the lowest-cost capable model, because it is also the model of unnamed subagents.

A saved profile therefore cannot quietly override a mandatory policy. Today the built-in **Thorough** profile (Coder on Sonnet) and **Local** profile (advisor off) are reported; that is a real conflict with the contracts, shown rather than hidden. The check reports; it does not change settings.

## Not implemented yet

- **No consumer.** No launch path calls `route`, `resolve`, `check_settings`, `assign_reviewer`, `validate_verdict` or `next_action` yet. The Claude adapter (#11) is the first.
- **Concurrency** is the per-role `max_concurrent` (1 to 4); there is no workflow-wide parallel cap.
- **Spend limits** and **durable repair loops** (state that survives a restart) are v0.9.
- **Pi, local models and other backends** are v0.7. `BackendProfile` is the place to describe each one.
- The Roles overlay's teammate settings remain instructions. Agentree cannot confirm them in a live session.
