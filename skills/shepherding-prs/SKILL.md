---
name: shepherding-prs
description: |
  Shepherd one shipit draft PR through review-addressing rounds. Use when an
  agent is assigned to a PR after it is open: read current review threads, classify
  findings, fix or reply, resolve threads, verify, push the round, and park
  without waiting or flipping ready.
---

# Shepherding PRs

Use this skill when you are the shepherd for one PR. You own review addressing for that PR across rounds. You are briefed cold once for round 1, parked between rounds, and resumed by the coordinator when a new round lands.

You never wait, coordinate, request reviews by hand, flip ready, or merge. The coordinator owns waits and the ready flip.

## Check The Cold Brief

Your round-1 brief should name:

- the PR and its `## Context` note;
- the issue the PR implements;
- exact verify commands for review fixes;
- governing PRD, ADR, or docs;
- decision boundaries that review comments cannot re-open.

If a mandatory slot is missing, flag it to the coordinator instead of guessing.

## Start Each Round From The PR

On every resume, re-read the current PR state, diff, and open review threads. Held context is only a head start; the PR is the source of truth for the round.

For every open thread:

- fix it, or reply with a clear rationale;
- classify the finding before pushing;
- resolve the thread.

Classify with:

```sh
shipit pr classify <pr> --comment <id> nitpick|substantive [--reason "<reason>"]
```

List unclassified findings with:

```sh
shipit pr classify <pr>
```

Use `nitpick` only for cosmetic wording, naming, or style that does not affect correctness, behavior, security, or maintainability. A reviewer's own `nit:` label is input, not a binding verdict.

## Sweep The Finding Class

A useful review finding is usually an instance of a broader class. Before pushing, scan the whole PR diff for other instances of the same class: the same stale reference, missing convention, escaping bug, incomplete rename, or repeated edge case. Fix the class in the same round when it is in scope.

If a thread tries to re-open a settled decision or asks for out-of-scope work, reply with the rationale, classify it, and resolve it. Do not unwind governing decisions unless the coordinator changes the boundary.

## Verify And Park

Run the exact verify commands from the brief after your fixes. In shipit, this is usually:

```sh
pixi run test
```

The commit and push hooks run the lint gate. Use `shipit lint --fix` only when formatting damage is expected, then commit and let the hook check it.

Push the round's commits together. Then hand back to the coordinator and park. Trust `shipit pr status` or `shipit pr next` for the next action; do not re-request reviews manually.
