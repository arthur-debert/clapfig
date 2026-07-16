---
name: planning
description: Plan a new feature or epic from loose ideas through to issues. Drives ideation → overview checkpoint → Spec → ADRs → docs PR (Leg A), then epic decomposition → issues (Leg B). Use at the START of a feature/epic, before any code. NOT for single fixes — the simpler-issue path skips planning.
---
# Planning

Conduct a planning session for a **new feature or epic**. This is the orchestrator: it drives the other planning skills in order and keeps the human in the loop at the three checkpoints that matter.

**Two legs, often two sessions.** Leg A turns loose ideas into the **Spec** — `docs/spec/<slug>.md`, authored first, then the **ADRs** that follow from it — locked by a merged docs PR. Leg B turns that Spec into **execution tracking** (epic issue(s) + Work Stream sub-issues). Leg B is independently invocable — it is frequently a separate, later session run directly once the Spec is merged. Run Leg A end-to-end first; only continue into Leg B when asked, or note it as the next session.

**Three artifacts, in dependency order — Spec → ADR → ticket.** Each sits at a distinct altitude and is produced only once the one above it exists:

| Artifact | Altitude | Question | Skill |
| --- | --- | --- | --- |
| **Spec** | why + general | *why are we doing this, and what, broadly?* | `/to-spec` |
| **ADR** | specific what | *given the Spec, which durable decisions, and why?* | `/grill-me-with-docs` |
| **ticket** | how | *how is it sliced and built?* | `/to-tickets` |

The rule that fixes this order: **no ADR before the Spec** — we don't decide *how* before we know *what*. The grill (which writes the ADRs — hard-to-reverse architectural decisions) does not run until the Spec has pinned the general shape of the feature.

**Spec vs tracker — keep these distinct:**

- **Spec** = the feature definition — the authoritative file in `docs/spec/`. The *what & why*.
- **Epic issue** = an execution tracker — a GitHub issue that points to the Spec + ADRs and carries a Spec summary plus the WS topology and progress. It is **not** the spec.

**Bail early on small work.** Planning is for features and epics. If this is a single fix or a small change, stop and say so — skip straight to implementation. Don't manufacture an epic for a one-PR change.

**Interview style.** For every interactive step (ideation, overview, decomposition), ask **one question at a time**, wait for the answer, and **recommend an answer** with each question — same cadence as `/grill-me-with-docs`.

---

## Leg A — Feature planning (the entry point)

### 1. Ideation

Open discussion. Move from loose ideas to a structured, shared understanding of the feature. Pull in specifics rather than guessing:

- **Outside research** is blessed — look things up when a decision turns on facts you don't have.
- **Codebase exploration** is blessed — delegate **Explore** agents to find how the relevant area works today, what's already there, and what would have to change.

**Name the feature here** — agree a short, memorable name; everything downstream (Spec slug, ADRs, later the epic) hangs off it.

### 2. Overview checkpoint (user checkpoint)

Present a **high-level overview** of the feature — the shape of it, the major pieces, the approach. The user **oks it or requests changes**. Loop until they ok. Do not proceed to the Spec (nor the grill that follows it) until the overview is blessed.

Once the overview is blessed, record the session's purpose in the dev-cycle log (best-effort — ADR-0032):

```sh
shipit log event session.intent --about "planning session: <feature name>"
```

If the command errors, continue — a skipped emission is a missing event, never a broken planning step. A later `session.intent` (e.g. restated when the grill starts) supersedes at read time.

### 3. Spec — `/to-spec`

Run `/to-spec`. It synthesizes the ideation + blessed overview into the **Spec file only** — `docs/spec/<slug>.md`, the authoritative *why/general-what* of the feature. This is the FIRST artifact: it pins **what** we're building, broadly, before any ADR decides **how**. No durable architectural decisions here (those belong in the ADRs, next); no requirements interview (that's the grill, next); no epic issue (that's Leg B).

### 4. Grill — `/grill-me-with-docs` (the ADR step)

Run `/grill-me-with-docs`: relentless one-question-at-a-time Q&A that grills **the Spec**, challenged against `CONTEXT.md` and the existing domain model. It sharpens terminology and writes **ADRs** for the specific, durable architectural decisions the Spec implies. The grill runs only AFTER the Spec — **no ADR before the Spec**; we don't decide *how* before we know *what*.

### 5. Docs PR (user checkpoint at merge)

Push the Spec + ADR changes as a DRAFT PR and run it through the **full required-reviewer cycle, exactly like code** — architectural oversights surface in review. The agent addresses review threads itself, **surfacing to the user only when a real call is needed**, flips the PR to **READY** when reviews are settled and CI is green, then stops. **The user merges.** The merged PR is what locks the spec.

---

## Leg B — Issue planning (independently invocable)

Run this once the Spec is merged — usually a fresh session. If invoked directly, read the merged Spec in `docs/spec/` first for context.

### 6. Epic naming + decomposition (user checkpoint)

The user proposes the **epic name(s)**. Work with them on a **terse, nested epic / WS list** — back-and-forth until they ok the decomposition.

One feature **may span several epics** (e.g. `OBS01`→`OBS04`: one feature, several epics, because a single mega-epic would be too large/slow to merge). **Default to one epic**; only split when the work is genuinely too big to land as one umbrella.

### 7. Issues — `/to-tickets`

Run `/to-tickets`, **per epic**. It creates:

- the **epic umbrella issue** — the execution tracker: Spec summary, pointers to the Spec + relevant ADRs, the WS list/topology, progress;
- the **WS sub-issues** — high detail (risks, where/how in the code, testing, links to the Spec/ADRs), formally linked as sub-issues for GitHub progress tracking.

---

## The checkpoints, in one place

Everything between the checkpoints the agent drives on its own authority. The human is on the hook at exactly three points:

1. **Overview ok** (step 2) — before the Spec (and the grilling that follows it).
2. **Docs PR merge** (step 5) — the spec is locked only by a human merge.
3. **Decomposition ok** (step 6) — before issues are filed.
