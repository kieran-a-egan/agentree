---
name: implement
description: Make a scoped code change to a plan task and report what changed. Use when a task names the files to edit and how it will be checked.
---

# Implement

## Inputs

- **task** (required): one plan task with its done-check.
- **files** (required): the files this agent may change; others are read-only.
- **plan** (optional): the surrounding plan, for context.

## Procedure

1. Read the code and its tests before editing.
2. Reuse existing helpers and match the surrounding style.
3. Change only the owned files; ask the lead if another file must change.
4. Add or update a test that fails if the new logic breaks.
5. Run the done-check and the project's tests for the area.

## Outputs

A report with:
- **Changes**: each file and what changed.
- **Verification**: commands run and their results, including failures.
- **Follow-ups**: anything left undone or out of the owned files.

## Limits

Leaves changes uncommitted in the working tree. Never stages, commits, pushes or rewrites history without explicit human authorization.
