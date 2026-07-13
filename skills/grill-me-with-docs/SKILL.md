---
description: Grilling session that challenges your plan against the existing domain model, sharpens terminology, and updates documentation (CONTEXT.md, ADRs) inline as decisions crystallise. Use when user wants to stress-test a plan against their project's language and documented decisions.
metadata:
    github-path: skills/engineering/grill-with-docs
    github-ref: refs/heads/main
    github-repo: https://github.com/mattpocock/skills
    github-tree-sha: 2c6aa7079cc705a6e0c319fcf23a0fa39968de29
name: grill-me-with-docs
---
<what-to-do>

Before the first question, record the grill in the dev-cycle log (best-effort — ADR-0032; on any error continue silently, a skipped emission is a missing event, never a broken step):

```sh
shipit log event session.intent --about "planning session: <topic>"
shipit log event planning.grill.started
```

Interview me relentlessly about every aspect of this plan until we reach a shared understanding. Walk down each branch of the design tree, resolving dependencies between decisions one-by-one. For each question, provide your recommended answer.

Ask the questions one at a time, waiting for feedback on each question before continuing.

If a question can be answered by exploring the codebase, explore the codebase instead.

</what-to-do>

<supporting-info>

## Domain awareness

During codebase exploration, also look for existing documentation:

### File structure

Most repos have a single context:

```text
/
├── CONTEXT.md
├── docs/
│   └── adr/
│       ├── 0001-event-sourced-orders.md
│       └── 0002-postgres-for-write-model.md
└── src/
```

If a `CONTEXT-MAP.md` exists at the root, the repo has multiple contexts. The map points to where each one lives:

```text
/
├── CONTEXT-MAP.md
├── docs/
│   └── adr/                          ← system-wide decisions
├── src/
│   ├── ordering/
│   │   ├── CONTEXT.md
│   │   └── docs/adr/                 ← context-specific decisions
│   └── billing/
│       ├── CONTEXT.md
│       └── docs/adr/
```

Create files lazily — only when you have something to write. If no `CONTEXT.md` exists, create one only when the first term qualifies for the curated glossary. If no `docs/adr/` exists, create it when the first ADR is needed.

## During the session

### Challenge against the glossary

When the user uses a term that conflicts with the existing language in `CONTEXT.md`, call it out immediately. "Your glossary defines 'cancellation' as X, but you seem to mean Y — which is it?"

### Sharpen fuzzy language

When the user uses vague or overloaded terms, propose a precise canonical term. "You're saying 'account' — do you mean the Customer or the User? Those are different things."

### Discuss concrete scenarios

When domain relationships are being discussed, stress-test them with specific scenarios. Invent scenarios that probe edge cases and force the user to be precise about the boundaries between concepts.

### Cross-reference with code

When the user states how something works, check whether the code agrees. If you find a contradiction, surface it: "Your code cancels entire Orders, but you just said partial cancellation is possible — which is right?"

### Capture glossary terms selectively

When a high-value domain term is resolved and it belongs in the shared glossary, update `CONTEXT.md` right there. Don't batch up qualifying glossary updates — capture them while the distinction is fresh. Use the format in [CONTEXT-FORMAT.md](./CONTEXT-FORMAT.md).

Updating `CONTEXT.md` is not the goal of the grill. The goal is sharper shared language and better decisions. Most resolved details should stay in the conversation, code, ADRs, Specs, or focused docs; only terms that are project-specific, repeatedly useful, and likely to be misunderstood without a glossary entry belong in `CONTEXT.md`.

`CONTEXT.md` should be totally devoid of implementation details. Do not treat `CONTEXT.md` as a spec, a scratch pad, a transcript, or a repository for implementation decisions. It is a curated glossary and nothing else. Do not add general computing terms, one-off labels, workflow status words, or every term mentioned during the grill.

### Offer ADRs sparingly

Only offer to create an ADR when all three are true:

1. **Hard to reverse** — the cost of changing your mind later is meaningful
2. **Surprising without context** — a future reader will wonder "why did they do it this way?"
3. **The result of a real trade-off** — there were genuine alternatives and you picked one for specific reasons

If any of the three is missing, skip the ADR. Use the format in [ADR-FORMAT.md](./ADR-FORMAT.md).

Each time an ADR file is written, record it in the dev-cycle log (best-effort; continue on error):

```sh
shipit log event planning.adr.written --about "ADR-NNNN: <title>"
```

</supporting-info>
