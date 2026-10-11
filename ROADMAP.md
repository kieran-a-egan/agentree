# Agentree Roadmap: v0.5 → v1.0

**Status:** Proposed roadmap · **Baseline:** v0.4.4, including the latest reviewed `main` branch changes · **Target:** a local-first, provider-independent agent engineering environment.

This roadmap describes release outcomes, not fixed dates. Each milestone ships only when its acceptance criteria pass. Agentree's primary user interface remains a Windows terminal application through v1.0; the orchestration and plugin contracts should be operating-system independent.

## Product principles

- **One interface, multiple runtimes.** Preserve Claude Code's native teams, while allowing Pi sessions and local models through separate adapters. Do not pretend different runtimes expose identical functionality.
- **Portable capabilities, explicit orchestration.** Use `AGENTS.md` for project instructions; Agent Plugins for skills and MCP resources; Agentree-owned role/workflow definitions for models, permissions, stages and handoffs.
- **Measured, not invented.** Distinguish observed usage from estimates and unknown data. Provide traceable events for agent states, costs and reviews.
- **Fail safely.** Human approvals, isolation, budget limits and escalation policies must be enforceable, not merely written in prompts.
- **Keep the working path working.** Maintain the existing `agentree` command, project/objective screen, Windows ConPTY Claude session, native teams and terminal interaction as the architecture evolves.

## Current baseline — v0.4.4

Agentree currently provides a Windows Ratatui dashboard, embedded Claude Code through ConPTY, native Claude-team observation, project/objective selection, activity and estimated usage telemetry, attention prompts, and model-directed Haiku/Opus roles. This is the baseline for compatibility tests, **not** proof that model assignment, final-review gates or cost accounting are independently enforced.

## v0.5 — Configuration & core contracts

**Status:** Implementation complete (#1–#5, #38); release gate #6 verified by `release_gate_*` tests and the checklist in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#v05-release-gate).

**Outcome:** Models and run policies are configurable without editing Rust prompts.

- Settings screen for each role: **lead, planner, explorer, researcher, coder, tester, advisor and final reviewer**.
- Per-role backend, model, effort (where supported), concurrency and permission profile; separate defaults for System One fallback.
- Global and per-project profiles (for example *Budget*, *Balanced*, *Local*, *Thorough*) with validation and safe migration of existing settings. Store credential references, never plaintext API keys.
- Backend **capability discovery**: supported models, tools, context limits, interactivity, team mode, advisor support and permissions. Invalid combinations must be disabled or explained.
- Define internal `Run`, `Agent`, `Task`, `Stage`, `Usage`, `Approval` and `Event` types. Begin separating UI rendering from the Claude-specific session monitor.
- Establish Windows CI for formatting, linting, unit tests and release builds.

**Release gate:** A user can change any supported stage's model in the UI, save a profile, restart Agentree and run with the chosen configuration; unsupported settings fail validation before launch. The existing Claude team workflow still works.

## v0.6 — Portable roles, Agent Plugins & AGENTS.md

**Outcome:** Role behaviour is defined once and reusable across runtimes.

- Load and validate **Agent Plugins v1.0** manifests, bundled skills, references and MCP server declarations; apply path containment and explicit plugin trust/enablement rules.
- Ship a built-in software-engineering plugin: **plan, explore, research, implement, test, advise, review**.
- Add Agentree role/workflow definitions mapping roles to plugin skills, output contracts, allowed tools and review/escalation rules. Keep these outside the Agent Plugins standard.
- Resolve project `AGENTS.md` instructions consistently; document coexistence with runtime-specific instruction files such as `CLAUDE.md` without silently overwriting them.
- Provide a Claude adapter that translates roles and permitted plugin capabilities into its native team/session workflow.
- Track which instruction files, plugins and skill versions were used for each run.

**Release gate:** A clean repository can start from `AGENTS.md` plus a reusable plugin and role profile. Different roles load the expected skills. Invalid or untrusted plugins do not execute. The old hardcoded role prompt is no longer the single source of truth.

## v0.7 — Multi-provider runtime & local models

**Outcome:** The same Agentree workflow runs on Claude Code or Pi, and selected roles can use local models.

- Introduce a `RuntimeAdapter` interface for launch, cancellation, events, user prompts, capability reporting, and resume where supported.
- Normalize agent/team events, tool activity, handoffs, outcomes and usage into a single event stream for the dashboard.
- Preserve Claude Code's native Agent Teams adapter rather than recreating its internal coordination.
- Add a **Pi adapter**, initially via its subprocess RPC mode, with a clear distinction between independent Pi sessions and native Claude teammates.
- Support providers reachable through Pi, including configured local/OpenAI-compatible endpoints (such as Ollama, LM Studio or vLLM), subject to tool and context capabilities.
- Enable per-role backend selection and limited cross-backend stage handoffs using explicit artifacts and structured task contracts.
- Add provider-health checks, credential detection and missing-capability errors before starting a run.

**Release gate:** The same example task and plugin skill complete through Claude Code and Pi. A local model can execute at least one supported read-only or coding role in a documented smoke test. Mixed-provider handoffs are attributed correctly; a Pi worker is never mislabelled as a native Claude teammate.

## v0.8 — System One decision-engine interface

**Outcome:** Cheap, fast, swappable decisions route work without requiring a full reasoning-agent turn every time.

- Define a platform-agnostic `DecisionEngine` contract: task/context summary + available capabilities + policy/budget constraints → **route, tool action, answer, defer, or escalate**, with confidence and rationale.
- Implement a **deterministic Rust reference engine** for low-risk dispatch, caching and policy checks; include a *disabled* mode.
- Add an external engine adapter protocol (versioned structured messages over process/stdin-stdout or RPC), so JEV, KEV, Laya or OpenJEV can be integrated when a usable interface exists. Do not claim these integrations until tested.
- Support routing strategies, threshold tuning, timeout/fallback handling and human confirmation for consequential actions.
- Record routing latency, engine selection, decisions, escalation rate, failures and estimated costs; build an evaluation corpus with expected outcomes.
- Permit System One to route across Claude, Pi and local backends **without** bypassing runtime tool permissions.

**Release gate:** The reference engine and one external adapter pass the same conformance suite. Failed or uncertain decisions reliably fall back to the configured reasoning agent. Every decision is auditable, and unauthorized actions cannot be dispatched.

## v0.9 — Reliability, governance & reproducibility

**Outcome:** Long-running, mixed-provider development work is recoverable, bounded and inspectable.

- Durable run state: task graph, stage transitions, outputs, approvals, budgets and event checkpoints.
- Pause, cancel and resume where backends support it; on restart, detect and explicitly report stages that cannot be resumed.
- Git worktree isolation and conflict-safe integration for parallel coding agents; optional checkpoint commits with user-controlled policy.
- Enforced limits for concurrency, tool permissions, time and estimated spend, with warning/stop thresholds and clear unknown-usage treatment.
- Structured review/repair loops: test → final review → bounded fix cycles → verified completion or actionable attention prompt.
- Timeline replay, event/export logs, model/skill provenance and sensitive-data redaction.
- Real-time event-driven activity indications; no stale “working” labels or made-up per-agent metrics.

**Release gate:** Simulate crashes, denied permissions, budget exhaustion, conflicting edits, stalled workers and interrupted reviews. Agentree recovers or fails explicitly without losing task state or silently continuing unsafe work.

## v1.0 — Stable public release

**Outcome:** A dependable, documented Windows release with stable provider and plugin contracts.

- Publish versioned `agentree.exe` builds via GitHub Releases, checksums and straightforward install/update/uninstall paths; Rust source installation remains available.
- Stabilize the plugin/role schema, runtime adapter and System One engine protocol, with versioning and compatibility tests.
- Provide documented capability matrices and examples for **Claude Code, Pi and a local-provider configuration**.
- Validate installation on a clean Windows environment and live end-to-end workflows: setup → delegation → attention → tests → review → completion.
- Complete security review for plugin supply chain, credential handling, subprocess execution, workspace isolation and private transcript retention.
- Ship sample profiles and a minimal built-in engineering plugin, developer documentation, contribution guidelines, migration notes and a changelog.
- Set release standards: CI green; no known critical data-loss or unsafe-permission bugs; clearly documented telemetry limitations; benchmark/regression thresholds maintained.

**Release gate:** A new user can install Agentree, configure a supported backend, run a real workflow, inspect work and costs, recover from normal interruptions, and understand the outcome without manually editing source files.

## Sequence and dependencies

| Release | Main dependency | Demonstration |
| --- | --- | --- |
| **0.5** | Reviewed Claude baseline | Choose and persist stage models |
| **0.6** | 0.5 role contracts | Same role capability packaged as a plugin |
| **0.7** | 0.6 plugin/role portability | Claude, Pi and local-model smoke tests |
| **0.8** | 0.7 runtime adapters | Swappable System One decisions and escalation |
| **0.9** | 0.8 event/policy contracts | Crash recovery, approvals and bounded repair |
| **1.0** | All release gates | Documented, installable, tested public release |

## Explicitly deferred beyond v1.0

Distributed/remote workers, a hosted control plane, multiplayer collaboration, a full graphical workflow editor, general-purpose browser automation, and a native macOS/Linux **UI**. The interfaces should not preclude these, but none is required for a stable v1.0.

## Release management

Use each version as a GitHub milestone and track its release gate as an acceptance issue. Prefer small PRs by subsystem; do not merge changes that break the existing Claude-only happy path. Update this roadmap when evidence changes scope, but keep release gates measurable.