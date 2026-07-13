---
name: to-spec
description: Turn the current conversation context into an authoritative feature Spec and write it to docs/spec/. Use when user wants to create a Spec from the current context after planning/grilling.
metadata:
    forked-from: https://github.com/mattpocock/skills (skills/engineering/to-prd)
---
This skill takes the current conversation context and codebase understanding and produces a Spec. Do NOT re-run the requirements interview - that happened earlier, in `/grill-me-with-docs`; synthesize the Spec from what you already know. This is not a fully AFK skill, though: step 2 still expects a short confirmation of the module boundaries and test scope with the user. That scoped confirmation is not a requirements interview.

The issue tracker and triage label vocabulary should have been provided to you - run `/setup-matt-pocock-skills` if not.

## Process

1. Explore the repo to understand the current state of the codebase, if you haven't already. Use the project's domain glossary vocabulary (`CONTEXT.md`) throughout the Spec, and respect any ADRs in the area you're touching.

2. Sketch out the major modules you will need to build or modify to complete the implementation. Actively look for opportunities to extract deep modules that can be tested in isolation.

   A deep module (as opposed to a shallow module) is one which encapsulates a lot of functionality in a simple, testable interface which rarely changes.

   Check with the user that these modules match their expectations. Check with the user which modules they want tests written for.

3. Write the Spec using the template below. **The Spec is the authoritative feature definition** - the *what & why*. It is a file, not an issue body:

   - Write it to `docs/spec/<slug>.md`. This file is the single source of truth for the feature definition.
   - That is the whole output of this skill. Do NOT open an epic tracker issue here. The **epic GitHub issue is an execution tracker** - it summarizes the Spec and points to it plus the relevant ADRs - and it is created later, in `/to-tickets` (the issue-planning leg), not by this skill.
   - The epic code (`THEME+NN`, e.g. `GPU02`) is assigned by the human, but it is used later in `/to-tickets` when the epic issue is minted - not here.

4. Once the Spec file is written, record the milestone in the dev-cycle log (best-effort - ADR-0032; if the command errors, continue - a skipped emission is a missing event, never a broken step):

   ```sh
   shipit log event planning.spec.written --about "Spec: docs/spec/<slug>.md"
   ```

<spec-template>

## Context

What exists today, what prior ADRs/docs constrain this work, and why this Spec exists now.

## Problem

The concrete failure mode or opportunity. Prefer observed facts, examples, and current costs.

## Goals

What must become true for the work to count as successful.

## Non-Goals

Plausible things we are explicitly not doing.

## Proposed Shape

The high-level solution. This should be understandable before reading module details.

## User / Agent Stories

A numbered list of stories covering actors, workflows, failure modes, and operator needs. Each story should usually use:

1. As an <actor>, I want a <feature>, so that <benefit>

## Design Decisions

The durable implementation-facing choices. This can include:

- Module boundaries
- Value objects, APIs, or state machines
- Data contracts
- CLI surfaces
- Persistence or logging behavior
- Compatibility and migration choices

Do NOT include incidental file paths or code snippets. They may end up being outdated very quickly.

Exception: if a prototype produced a snippet that encodes a decision more precisely than prose can (state machine, reducer, schema, type shape), inline it within the relevant decision and note briefly that it came from a prototype. Trim to the decision-rich parts - not a working demo, just the important bits.

## Alternatives Considered

The credible rejected options and why they lose in this context.

## Risks And Rabbit Holes

Known traps, ambiguity, sequencing hazards, or places implementers are likely to overbuild.

## Cross-Cutting Concerns

Security, privacy/secrets, observability/logging, CI/release, migration, compatibility, and performance concerns that affect the design.

## Testing / Verification

What good tests assert, which modules are tested, prior-art tests to copy, and any live/acceptance evidence required.

## Workstream Hints

Optional. Only enough to help `/to-tickets`; not a full issue breakdown.

## Out Of Scope

Hard boundaries for this Spec.

## Further Notes

Supersession notes, historical context, links, and follow-up hooks.

</spec-template>
