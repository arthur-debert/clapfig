---
name: coordinating
description: |
  Coordinate shipit's delegated dev cycle. Use when the top-level agent is
  orchestrating a standalone issue, epic, workstream set, or PR lifecycle:
  gather context, brief implementers and shepherds, drive the PR engine, wait,
  flip ready, and stop at the human merge gate.
---

# Coordinating

Use this skill when you are the top-level shipit coordinator. Your job is orchestration, not implementation. You may write planning artifacts such as PRDs, ADRs, and CONTEXT.md updates, but you do not edit code paths for a change. Delegate every implementation, regardless of size.

## Orient

Start from the issue, handoff, or maintainer instruction. Read enough code and docs to brief the work accurately before spawning anyone. For existing sessions or epics, read the event log:

```sh
shipit logs --flow --session current
shipit logs --flow --epic <CODE> --agent-ids
```

Use `shipit pr status` and `shipit pr next` as the authoritative PR engine. Do not re-derive reviewer, wait, or breaker policy from memory.

## Spawn Implementers

For each unit of work, launch an implementer in an isolated Tree. Do not hand-run `shipit tree create` for a Run, and do not point an agent at an arbitrary checkout.

Standalone issue:

```sh
shipit spawn subagent --repo <owner/repo> --issue <N> --role implementer
```

This cuts `issues/<id>/<session>` from `origin/main`, with `session` defaulting to `work`, and the draft PR targets `main`.

Epic workstream:

```sh
shipit spawn subagent --repo <owner/repo> --epic <EPIC> --ws <N> --issue <I> --role implementer
```

This cuts `EPIC/WSnn` from `origin/EPIC/umbrella`, and the draft PR targets `EPIC/umbrella`. If the umbrella branch is missing, the spawn must fail loudly, not fall back to `main`.

When you brief an implementer, print the task template and fill every slot:

```sh
shipit spawn brief implementer
```

Name the issue, exact verify commands, governing PRD/ADR/docs, and decision boundaries. Never leave a slot blank and never ask the implementer to guess verification.

## Drive Each PR

The implementer opens a draft PR, links the issue, writes a `## Context` handoff note, runs `shipit pr next` once, and stops. From there:

1. Own every wait with `shipit pr wait --until reviews-in|ready`.
2. Spawn one shepherd per PR for review addressing.
3. Resume the same shepherd for later rounds with a one-line brief that restates the engine's current verdict.
4. Run `shipit pr status` or `shipit pr next` after each handoff and do the one next action the engine reports.
5. When the engine reports READY, run `shipit pr ready`.
6. Stop at the ready flip. The human merges.

Only merge on your own authority when integrating a READY workstream PR into its epic branch. Never merge a standalone PR or an epic umbrella PR into `main`.

## Run Epics

An epic is the same role split with different branch topology. The topology is fixed, not a menu.

- Create the epic branch as `EPIC/umbrella` off `origin/main`.
- Spawn implementers for eligible workstreams according to the dependency graph.
- Run workstreams in parallel where possible, but integrate READY workstream PRs into the epic branch one at a time.
- After each integration, re-check still-open workstream PRs for conflicts or required rebases.
- After planned workstreams land, run convergence for epic-owned fallouts, then a docs pass.
- Open the umbrella PR from `EPIC/umbrella` to `main`, shepherd it through the same draft cycle, flip ready, and stop for the human merge.

Epic and repo codes are assigned by the human. Derive branch and title forms from the assigned codes: `EPIC/WSnn` for workstream branches, `EPIC/umbrella` for the epic branch, and `<REPO>-<EPIC>-WSnn: Epic: <Epic Name> - Workstream: <WS Name>` for epic workstream titles.

## Preserve Durable Learning

Before wrapping a session or epic, move durable learning into the repo. Process rules belong in role `.lex` files or `docs/dev/`; decisions belong in ADRs; vocabulary belongs in `CONTEXT.md`; unresolved work belongs in tracker issues. Session memory is scratch space, not an archive.
