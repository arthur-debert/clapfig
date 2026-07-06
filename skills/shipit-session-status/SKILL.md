---
name: shipit-session-status
description: Show the dev-cycle story of the current session — the flow view of spawn/review/planning milestones from shipit's durable log. Use when the user asks what happened this session, wants a session status, or asks about an epic's progress.
---
# Session status

Render the current session's dev-cycle story — one command, no flags to remember:

```sh
shipit logs --flow --session current
```

Asked about an **epic** rather than this session (e.g. "where is RVW01?"), filter by the epic code instead — epic events span sessions:

```sh
shipit logs --flow --epic <CODE>
```

## What you get

The flow view (LOG04 / ADR-0032): a header line — the session's stated intent when a `session.intent` event exists, otherwise a theme inferred from the epics seen — then one line per dev-cycle event with a friendly relative time and an `EPIC-WSnn:` prefix. `--flow` implies `--events`, so only milestone records appear.

## Notes

- Add `--agent-ids` when the user wants to know which agent did what — agent ids are always collected, displayed only on request.
- Filters compose as AND: `--epic <CODE> --ws <n>` narrows to one Work Stream; `--pr <n>` to one PR.
- Read-only. A log with no records yet (or no matching records) is reported plainly, not an error.
- Print the view for the user; if it is long, follow it with a two-or-three-line summary of where things stand.
