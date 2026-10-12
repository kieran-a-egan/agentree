---
name: review
description: Audit changed code and its validation evidence at the end of a run and return a verdict. Use for the final independent review before work is called complete.
---

# Review

## Inputs

- **diff** (required): the changed files or patch.
- **objective** (required): what the change was meant to achieve.
- **evidence** (required): test and build output the team observed.
- **plan** (optional): the plan the change followed.

## Procedure

1. Read the whole change against the objective.
2. Check correctness, security, data loss and test coverage; confirm the evidence supports the claims.
3. Rank findings by severity, each with a failure scenario and a fix.
4. Say plainly if the evidence is missing or does not cover the change.

## Outputs

A report with:
- **Verdict**: `PASS`, `NEEDS_CHANGES` or `BLOCKED`.
- **Findings**: most severe first; each has file, failure scenario and fix.
- **Evidence gaps**: claims the validation does not support.

## Limits

Read-only: never edits production code. `PASS` requires the evidence to cover the change; otherwise use `NEEDS_CHANGES`, or `BLOCKED` when review could not be performed. The run's own lifecycle lines are emitted by the lead as its instructions specify, not by this skill.
