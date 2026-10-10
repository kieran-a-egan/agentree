# Event schema (v0.5 contract)

## Purpose

This document defines the runtime-neutral contract in `src/model.rs`. Adapters translate native runtime data into these types. Claude Code is the first adapter. The dashboard consumes these types and does not read runtime files directly.

## Schema version and policy

Current version: `EVENT_SCHEMA_VERSION = 1`, stamped on every event as `schema_version`.

- Adding an optional field keeps version 1.
- Removing or renaming a field, or changing a tag, bumps `EVENT_SCHEMA_VERSION`.
- Adding an event kind also bumps `EVENT_SCHEMA_VERSION`. Event kinds are an internally tagged enum with no fallback variant, so a version 1 reader rejects an unknown `type`.
- Consumers must explicitly reject or ignore any `schema_version` they do not know. They must not guess. The `Event` deserialiser in `src/model.rs` rejects any value other than `1`.

## Event envelope

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | Contract version. `1` for this document. |
| `id` | string | Unique within a run. See [IDs](#ids). |
| `run_id` | string | Run this event belongs to. |
| `agent_id` | string or `null` | Agent that produced the event. `null` for run-level events such as the advisor. |
| `actor` | string | Human-readable label, e.g. `main`, a teammate name, `Opus advisor`. |
| `at` | integer or `null` | Unix epoch milliseconds, UTC. `null` when the source timestamp is absent or unparseable. |
| `kind` | object | Event payload, tagged by `type`. See [Event kinds](#event-kinds). |

## Event kinds

| Tag (`type`) | Fields | Emitted by the Claude adapter when |
| --- | --- | --- |
| `tool_used` | `tool` (string), `target` (string, may be empty) | A `tool_use` block appears in a local JSONL transcript. `target` is a sanitised label: a basename or pattern for paths, the host and port only for URLs, never raw command text, query strings or userinfo. |
| `advisor_invoked` | none | The server advisor tool use is seen. |
| `advisor_completed` | none | The advisor tool result is seen. |
| `review_started` | none | The lead emits the run-scoped `AGENTREE_REVIEW_START` line. |
| `objective_completed` | none | The run-scoped `AGENTREE_OBJECTIVE_COMPLETE` line is emitted. Lead only. |

## Measure semantics

A `Measure<T>` is either observed or unknown. It serialises as an explicit state:

- Observed: `{"state":"observed","value":<T>}`
- Unknown: `{"state":"unknown"}`

Unknown is never encoded as `0`. A zero means the source reported zero. A usage field the transcript does not report is Unknown, even when other fields in the same call are present. Whole-usage absence (no observed call) makes every field Unknown.

`Usage` fields are all measures and default to unknown. Example with one observed field and four unknown fields:

```json
{
  "input_tokens": {"state": "observed", "value": 1200},
  "output_tokens": {"state": "unknown"},
  "cache_read_tokens": {"state": "unknown"},
  "cache_write_tokens": {"state": "unknown"},
  "cost_usd": {"state": "unknown"}
}
```

`cost_usd` is an estimated USD list-price cost. It stays unknown when pricing is incomplete.

The Claude adapter reports usage as unknown until it has observed at least one transcript call with usage. Within the observed calls, a token field that any call omits is reported as unknown; the dashboard's displayed sum still counts only what was reported. The adapter reports `cost_usd` as unknown when any token field is missing, when the cache-write 1h/5m split is missing for a call that wrote cache, or when any observed model has no list price in the pricing table. The partial total still appears in the dashboard, but not in the contract.

## Timestamps

Timestamps are Unix epoch milliseconds, UTC. A timestamp is `null` when the source is absent or cannot be parsed. Adapters never substitute the current time.

The Claude adapter accepts only `YYYY-MM-DDTHH:MM:SS` with an optional `.fraction` and a literal `Z`. Offsets and any other form yield `null`.

## Entities

| Entity | Meaning |
| --- | --- |
| `Run` | One orchestrated attempt at a project objective. Status is `active`, `objective_complete` or `ended`. |
| `Agent` | A participant in a run: the lead, a teammate or a subagent. Carries a `link`. |
| `Task` | A unit of work assigned to an agent. Status is `pending`, `in_progress` or `completed`. |
| `Stage` | A phase of a run, `work` or `final_review`. Status is `pending`, `running`, `passed`, `needs_changes`, `blocked` or `complete`. |
| `Approval` | A request for a human decision. No adapter produces these yet. |

The Claude adapter reports a final-review stage as `passed` only when an advisor result or usage is observed in the transcript between the review-start marker and the verdict line. Advisor calls after the verdict do not count. A lead's `PASS` line alone leaves the stage `running`.

### Agent link values

| Value | Meaning |
| --- | --- |
| `native_verified` | Confirmed as a native teammate by the runtime. |
| `native_project_linked` | Matched to a native teammate by project metadata only. |
| `unverified` | Seen in telemetry but not tied to a native teammate. |
| `background_subagent` | A background subagent. |

Background subagents are never presented as native teammates. The dashboard must label them as `background_subagent`, whatever else it knows.

## IDs

IDs are opaque strings. They are unique within their scope: event IDs within a run, agent IDs within a run, and so on. Consumers must not parse them.

The Claude adapter derives:

- Agent IDs: `<run_id>/<name>`. The lead is `<run_id>/main`. Work from a transcript that cannot be attributed to a named agent has `agent_id: null` and actor `unattributed`, so unrelated workers are never merged into one ID.
- Event IDs: `<transcript path>@<byte offset>#<index>`, where `<index>` counts the earlier events from the same transcript line. The path uses the platform separator, so on Windows it contains backslashes. Each transcript line is consumed once, and the same line gives the same IDs whether it is read alone or in a larger batch.
- Task IDs: `<run_id>/task/<task file stem>`. The stem is the name of the task's file in the team's task directory, which is unique within that team and does not change when the task's status or subject changes. A task with no recorded file stem has no task ID and is not emitted.

No adapter produces `Approval` records yet, so their ID scheme is not defined.

## Example event

```json
{
  "schema_version": 1,
  "id": "C:/Users/me/.claude/projects/app/session.jsonl@48213#0",
  "run_id": "run-1",
  "agent_id": "run-1/main",
  "actor": "main",
  "at": 1781000000000,
  "kind": {"type": "tool_used", "tool": "Read", "target": "main.rs"}
}
```
