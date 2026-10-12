---
name: test
description: Verify a change by running and writing tests, then report what passed, failed or was not checked. Use after implementation or to reproduce a bug.
---

# Test

## Inputs

- **change** (required): the diff, files or behaviour to verify.
- **plan** (optional): the done-checks the plan named.
- **commands** (optional): how the project builds and tests.

## Procedure

1. Run the existing tests for the affected area first.
2. Add tests for behaviour the change introduces or fixes, including edge cases.
3. Distinguish a failure in the change from a failure that was already there.
4. Report results as observed; do not weaken a test to make it pass.

## Outputs

A report with:
- **Result**: pass, fail or partial.
- **Commands**: each run with its outcome and the output of any failure.
- **Tests added**: files and what each covers.
- **Not verified**: what could not be run and why.

## Limits

May add or edit test files only. Production code defects are reported, not fixed.
