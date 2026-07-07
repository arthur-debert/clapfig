---
name: shipit-to-prd
description: Turn the current conversation context into a PRD — the authoritative feature spec — and write it to docs/prd/. Use when user wants to create a PRD from the current context.
metadata:
    forked-from: https://github.com/mattpocock/skills (skills/engineering/to-prd)
---
This skill takes the current conversation context and codebase understanding and produces a PRD. Do NOT re-run the requirements interview — that happened earlier, in `/shipit-grill-with-docs`; synthesize the PRD from what you already know. This is not a fully AFK skill, though: step 2 still expects a short confirmation of the module boundaries and test scope with the user. That scoped confirmation is not a requirements interview.

The issue tracker and triage label vocabulary should have been provided to you — run `/setup-matt-pocock-skills` if not.

## Process

1. Explore the repo to understand the current state of the codebase, if you haven't already. Use the project's domain glossary vocabulary (`CONTEXT.md`) throughout the PRD, and respect any ADRs in the area you're touching.

2. Sketch out the major modules you will need to build or modify to complete the implementation. Actively look for opportunities to extract deep modules that can be tested in isolation.

A deep module (as opposed to a shallow module) is one which encapsulates a lot of functionality in a simple, testable interface which rarely changes.

Check with the user that these modules match their expectations. Check with the user which modules they want tests written for.

3. Write the PRD using the template below. **The PRD is the authoritative feature definition / spec** — the *what & why*. It is a file, not an issue body:

   - Write it to `docs/prd/<slug>.md`. This file is the single source of truth for the spec.
   - That is the whole output of this skill. Do NOT open an epic tracker issue here. The **epic GitHub issue is an execution tracker** — it summarizes the PRD and points to it plus the relevant ADRs — and it is created later, in `/shipit-to-issues` (the issue-planning leg), not by this skill.
   - The epic code (`THEME+NN`, e.g. `GPU02`) is assigned by the human, but it is used later in `/shipit-to-issues` when the epic issue is minted — not here.

4. Once the PRD file is written, record the milestone in the dev-cycle log (best-effort — ADR-0032; if the command errors, continue — a skipped emission is a missing event, never a broken step):

   ```sh
   shipit log event planning.prd.written --about "PRD: docs/prd/<slug>.md"
   ```

<prd-template>

## Problem Statement

The problem that the user is facing, from the user's perspective.

## Solution

The solution to the problem, from the user's perspective.

## User Stories

A LONG, numbered list of user stories. Each user story should be in the format of:

1. As an <actor>, I want a <feature>, so that <benefit>

<user-story-example>
1. As a mobile bank customer, I want to see balance on my accounts, so that I can make better informed decisions about my spending
</user-story-example>

This list of user stories should be extremely extensive and cover all aspects of the feature.

## Implementation Decisions

A list of implementation decisions that were made. This can include:

- The modules that will be built/modified
- The interfaces of those modules that will be modified
- Technical clarifications from the developer
- Architectural decisions
- Schema changes
- API contracts
- Specific interactions

Do NOT include specific file paths or code snippets. They may end up being outdated very quickly.

Exception: if a prototype produced a snippet that encodes a decision more precisely than prose can (state machine, reducer, schema, type shape), inline it within the relevant decision and note briefly that it came from a prototype. Trim to the decision-rich parts — not a working demo, just the important bits.

## Testing Decisions

A list of testing decisions that were made. Include:

- A description of what makes a good test (only test external behavior, not implementation details)
- Which modules will be tested
- Prior art for the tests (i.e. similar types of tests in the codebase)

## Out of Scope

A description of the things that are out of scope for this PRD.

## Further Notes

Any further notes about the feature.

</prd-template>
