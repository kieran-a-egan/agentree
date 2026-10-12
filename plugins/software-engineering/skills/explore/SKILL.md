---
name: explore
description: Locate and summarise the code relevant to a question without changing it. Use to find where something lives, how a flow works, or what a change would touch.
---

# Explore

## Inputs

- **question** (required): what to find out, for example "where is trust checked before a plugin runs?".
- **scope** (optional): folders or file patterns to stay within.
- **depth** (optional): `quick` (default) or `thorough`.

## Procedure

1. Search by name, then by usage, before reading whole files.
2. Trace the real flow end to end, including callers.
3. Record only what you verified in the files.

## Outputs

A report with:
- **Answer**: the direct answer, in a few sentences.
- **Evidence**: `file:line` references for every claim.
- **Related**: other code a change here would affect.
- **Unknowns**: what could not be confirmed.

## Limits

Read-only. Reports what the code does, not what it should do. Does not browse the web; use research for that.
