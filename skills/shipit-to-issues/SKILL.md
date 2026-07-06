---
name: shipit-to-issues
description: Break a PRD into one or more epic tracker issues plus independently-grabbable Work Stream sub-issues on the project issue tracker, using tracer-bullet vertical slices. Use when user wants to convert a plan/PRD into epics and work streams, create implementation tickets, or break down work.
metadata:
    forked-from: https://github.com/mattpocock/skills (skills/engineering/to-issues)
---
# To Issues

Break a plan into independently-grabbable **Work Streams (WS)** using vertical slices (tracer bullets). Each Work Stream ships as one PR.

The issue tracker and triage label vocabulary should have been provided to you — run `/setup-matt-pocock-skills` if not.

## Process

### 1. Gather context

Work from whatever is already in the conversation context. The authoritative source is the **PRD file in `docs/prd/`** (the feature spec — the *what & why*), produced by `/shipit-to-prd`. Read it in full, along with the ADRs it references. If the user passes a reference (PRD path, issue number, or URL) as an argument, fetch it and read it fully.

This skill **creates the epic umbrella issue(s)** — do not assume one already exists. The epic issue is an **execution tracker** (PRD summary + pointers to the PRD/ADRs + the WS topology), not the spec; the PRD file stays authoritative. (`/shipit-to-prd` writes the PRD only; epic-issue creation lives here.)

### 2. Explore the codebase (optional)

If you have not already explored the codebase, do so to understand the current state of the code. Issue titles and descriptions should use the project's domain glossary vocabulary (`CONTEXT.md`), and respect ADRs in the area you're touching.

### 3. Draft the epic(s) and their Work Streams (vertical slices)

First, settle the **epic decomposition**. One feature usually maps to a single epic, but it may split into a **list of epics** when a single mega-epic would be too large or slow to merge (e.g. `OBS01`→`OBS04`: one feature, several epics shipped in sequence). Default to one epic; propose more only when the size/merge argument is real. Each epic name (`THEME+NN`, e.g. `OBS04`) is assigned by the human; the WS codes under it are assigned here.

Then, for each epic, break its slice of the plan into **tracer bullet** Work Streams. Each WS is a thin vertical slice that cuts through ALL integration layers end-to-end, NOT a horizontal slice of one layer.

<work-stream-rules>
- Each WS delivers a narrow but COMPLETE path through every layer (schema, API, UI, tests)
- A completed WS is demoable or verifiable on its own
- Size each WS as the **thinnest *coherent, independently reviewable* PR** — prefer thin, but each WS must stand on its own as one reviewable PR, not a sub-fragment of one
- WS may touch overlapping files; do not try to make them file-disjoint (that turns slicing into an NP-hard problem). File contention is resolved at merge time, not by pre-partitioning.
- Favor making WS01 a **walking skeleton**: the thinnest end-to-end thread that proves the architecture is wired together
</work-stream-rules>

All Work Streams are AFK (implemented and merged by agents without mid-stream human interaction). The only human checkpoints are the upstream PRD approval and the final epic→main merge.

### 4. Quiz the user

Present the proposed breakdown as a numbered list, grouped under each epic. For each WS, show:

- **Title**: short descriptive name
- **Blocked by**: which other WS (if any) must complete first
- **User stories covered**: which user stories this addresses (if the source material has them)

Ask the user:

- Is the epic split right? (one epic vs. several — too coarse / too fine)
- Does the WS granularity feel right? (too coarse / too fine)
- Are the dependency relationships correct?
- Should any WS be merged or split further?

Iterate until the user approves the breakdown. Before publishing, confirm the human-assigned epic code(s) (`THEME+NN`, e.g. `OBS04`) for every epic — do not invent one. Without a confirmed code there is no valid `<EPIC>` for the epic and WS titles.

### 5. Publish the epic(s) and Work Streams to the issue tracker

Work one epic at a time. For each approved epic:

**a. Create the epic umbrella issue first.** This is the **execution tracker**, not the spec — use the epic template below. Title: `<REPO>-<EPIC>: Epic: <Epic Name>`. It carries a summary of the PRD, pointers to the authoritative PRD (`docs/prd/…`) and the relevant ADRs, and the WS list/topology. Apply the correct triage label unless instructed otherwise. Once the umbrella issue exists, record it in the dev-cycle log (best-effort — ADR-0032; continue on error):

```sh
shipit log event planning.epic.minted --about "<EPIC>: <Epic Name> (#<issue>)"
```

**b. Then publish its Work Streams** as sub-issues of that epic umbrella (formal sub-issue links improve progress tracking in the GitHub UI). Use the `<ws-template>` below. These are considered ready for AFK agents, so publish them with the correct triage label unless instructed otherwise. Publish in dependency order (blockers first) so you can reference real issue identifiers in the "Blocked by" field. After each WS sub-issue is created, record it (best-effort; continue on error):

```sh
shipit log event planning.ws.minted --about "<EPIC>-WSnn: <WS title> (#<issue>)"
```

The WS code (`WSnn`, scoped per epic+repo) is assigned here; the epic code comes from the human. Use the identifier in the WS title: `<REPO>-<EPIC>-<WSnn>: Epic: <Epic Name> - Workstream: <WS Name>`.

If the decomposition produced **multiple epics**, repeat (a)+(b) for each — one umbrella per epic, with its own WS sub-issue tree.

<epic-template>
## Summary

A short summary of the PRD — the *what & why*, enough to orient without opening the spec. This issue is an execution tracker, NOT the authoritative spec.

## Spec

- **PRD**: a reference to the authoritative feature PRD file read in step 1 (`docs/prd/<feature-slug>.md`). When one feature splits into several epics, every epic points at the same feature PRD.
- **ADRs**: references to the relevant ADRs.

## Work Streams

The WS list and topology for this epic — each WS, and which WS it is blocked by (parallelizable vs. sequential). Sub-issues are linked below for progress tracking.

</epic-template>

<ws-template>
## Parent

A reference to the epic umbrella issue created in step 5a.

## What to build

A concise description of this vertical slice. Describe the end-to-end behavior, not layer-by-layer implementation.

Avoid specific file paths or code snippets — they go stale fast. Exception: if a prototype produced a snippet that encodes a decision more precisely than prose can (state machine, reducer, schema, type shape), inline it here and note briefly that it came from a prototype. Trim to the decision-rich parts — not a working demo, just the important bits.

## Acceptance criteria

- [ ] Criterion 1
- [ ] Criterion 2
- [ ] Criterion 3

## Blocked by

- A reference to the blocking ticket (if any)

Or "None - can start immediately" if no blockers.

</ws-template>

Do NOT close any epic umbrella issue you create — it stays open as the live execution tracker until its WS sub-issues all land.
