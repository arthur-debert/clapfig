---
name: implementing
description: |
  Implement one shipit task as a role-scoped implementer. Use when an agent is
  spawned to build a standalone issue or epic workstream: make the code/test
  change in its Tree, verify it, open a draft PR with context, run the PR engine
  once, then stop before review rounds.
---

# Implementing

Use this skill when you are the implementer for one issue or workstream. You build and test the change, open the draft PR, run the PR engine once, and stop. You do not coordinate, address review rounds, wait for reviewers, flip ready, or merge.

## Check The Brief

Your brief should name:

- the issue this Run implements;
- exact verify commands;
- governing Spec, ADR, or docs to self-check against;
- decision boundaries and out-of-scope items.

If a mandatory slot is missing, flag it in your handoff and in the PR `## Context` note instead of guessing. Work inside the Tree and branch you were given. A standalone issue branch is `issues/<id>/<session>` from `origin/main`; an epic workstream branch is `EPIC/WSnn` from `origin/EPIC/umbrella`.

## Build The Change

Read the issue and governing docs before editing. For bugs, write the failing test first, then fix the root cause, not just the reported instance. Keep the diff scoped to the issue and decision boundaries.

When you change behavior, signatures, arguments, returns, or module contracts, update the affected docstring in the same diff. Also update caller docstrings or comments that describe the altered behavior.

## Verify

Run the exact verify commands from the brief before opening the PR. In shipit, the usual manual suite is:

```sh
pixi run test
```

The commit and push hooks run the lint gate. Do not run a separate lint check unless the brief asks for it or you expect formatting damage. If formatting is likely, run:

```sh
shipit lint --fix
```

Then commit and let the hook be the check.

## Open The Draft PR

Commit, push, and open one draft PR against the same base you branched from. Link the issue with `closes #N` when the PR targets `main`, or `for #N` when it targets an epic branch and must not auto-close on workstream merge.

The PR body must include a `## Context` handoff note for the future shepherd:

- why this approach;
- what governing docs you checked;
- what is out of scope;
- what reviewers should not re-open.

After the draft PR is open, run the engine once from the PR branch:

```sh
shipit pr next
```

This lets the engine place initial review requests. Run it once only. Do not loop, address reviews, wait, or flip ready. Hand back to the coordinator.
