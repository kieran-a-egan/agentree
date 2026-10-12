---
name: plan
description: Turn an objective into an ordered, testable work plan with file ownership. Use when work needs breaking down before anyone edits code.
---

# Plan

## Inputs

- **objective** (required): what is to be achieved, as written by the human or lead.
- **context** (optional): findings from the explore or research skills, project instructions (`AGENTS.md`, `CLAUDE.md`), constraints and non-goals.

## Procedure

1. Restate the objective and list what is out of scope.
2. Read the files the objective touches before proposing changes.
3. Split the work into tasks small enough for one agent, each with its own files so parallel tasks do not collide.
4. Order the tasks by dependency and name how each will be verified.

## Outputs

A plan in Markdown, in this order:
- **Goal**: one sentence.
- **Tasks**: numbered; each has an owner role, the files it may change, and a done-check.
- **Risks and open questions**: anything that needs a human decision.
- **Verification**: the commands or checks that prove the objective is met.

## Limits

Read-only. The plan never edits files and does not claim work is done. An open question that blocks the plan is raised to the lead, not guessed.
