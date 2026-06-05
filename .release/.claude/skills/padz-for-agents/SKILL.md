---
name: padz-for-agents
description: |
  Use padz to record, retrieve, and hand off notes and tasks across agent sessions.
  Trigger when: (1) the user asks to "save this as a note", "remember this for later",
  "turn this into a task / todo list"; (2) the agent needs to persist context across
  a compaction or handoff; (3) the user asks what's been noted, what's planned, what's
  done; (4) the repo already has a .padz/ directory and the user refers to "my notes"
  or "my pads". Not for working ON padz itself — see the padz repo's own
  `.claude/skills/` (padz-output, padz-display-identifiers, etc.) for that.
---

# padz for agents

padz is a CLI note + task tool. It stores **pads** — text files with metadata — in a project `.padz/` directory or a global store. You, the agent, should use padz whenever the user wants something remembered, listed, or handed off to a future session.

## The two non-negotiables

1. **Always pass `--no-editor` when creating pads.** In the default `notes` mode, `padz create` opens `$EDITOR`. For an agent that's a hang.
2. **Always pass `--output json` when reading pads.** The terminal renderer elides content for width. JSON is the full structured shape.

Everything else is optional — these two are load-bearing.

## JSON output shape

Every `--output json` response has this skeleton:

```json
{
  "messages": [ { "level": "info|success|warning|error", "content": "..." } ],
  "pads": [
    {
      "index":    { "type": "Regular|Pinned|Archived|Deleted", "value": 1 },
      "pad": {
        "content":  "<full body>",
        "metadata": {
          "id":          "1f7d95d5-55c6-4dda-a688-199d91f4fd2f",
          "title":       "...",
          "status":      "Planned|InProgress|Done",
          "tags":        ["..."],
          "parent_id":   null,
          "is_pinned":   false,
          "created_at":  "2026-04-24T23:26:28Z",
          "updated_at":  "2026-04-24T23:31:57Z"
        }
      },
      "children": [ /* same shape, recursive */ ],
      "matches":  null
    }
  ]
}
```

`content` can be large (pads are full notes). On `list` it's always present; use `peek` if you only need a preview. On `view <id>` it's the primary payload.

Some commands (create, delete, update) return mutated pads in an `affected_pads` key instead of `pads`. Same shape.

## DI vs UUID — the one gotcha that matters

Pads have two identifiers:

- **Display Index (DI)** — short, rank-based: `1`, `2`, `p1`, `d3`, `1.2`. Shifts as pads are added, deleted, or pinned.
- **UUID** — stable, opaque: `1f7d95d5-55c6-4dda-a688-199d91f4fd2f`. Never changes.

**Rule:** anytime a pad reference crosses a time boundary — saved in another pad's body, written to a plan file, referenced in a future session — store the UUID, not the DI.

Short UUID prefixes (8+ hex chars) work as selectors interchangeably with DIs, so you don't need to paste the whole thing:

```bash
padz list --uuid --output json              # get UUIDs alongside DIs
padz view 1f7d95d5 --output json            # 8-char prefix — fine
padz update 1f7d95d5-55c6-4dda-a688-...     # full UUID — also fine
padz delete 3                                # DI — fine within the same turn, risky later
```

If you're unsure whether a reference will outlive the current turn, use the UUID.

## Context-budget escalation ladder

Pads can be long. For agents with a context budget, three escalation steps:

```bash
padz list --output json              # titles only (cheapest)
padz list --peek --output json       # titles + ~200 char content preview
padz view <id> --output json         # full content (most expensive)
```

Rule of thumb: **list → peek → view.** Don't jump to `view` across many pads at once — you can burn the context window on pads you didn't need. Peek first, decide, then view.

For handoff to another session or another machine, `export` bundles selected pads into a single tar.gz that `import` can ingest elsewhere:

```bash
padz export 1 2 3 --output json          # writes padz-<timestamp>.tar.gz
padz import path/to/archive.tar.gz       # restores pads + metadata
```

## Notes mode vs todos mode

Single config toggle: `mode = "notes"` (default) or `mode = "todos"` in `.padz/padz.toml` or via `padz config set mode todos`.

| | notes (default) | todos |
|---|---|---|
| `create` default | opens `$EDITOR` | `--no-editor` implicit |
| Status icons in list | hidden | shown |
| `complete` behavior | marks Done **and deletes** | marks Done, pad stays |
| Typical pad size | paragraphs / pages | single line |

Everything else (tags, archive, pin, move, parent/child, search) works identically in both modes. `padz list --show-status` forces status icons on even in notes mode.

As an agent, pass `--no-editor` on `create` regardless of mode — your capture flow should never depend on the user's config.

## Scopes

Two stores:

- **Project** (default): `.padz/` in the project root, resolved by walking up from cwd.
- **Global** (`-g` / `--global`): OS user config dir. Cross-project scratchpad.

Always check whether the user means the project or the global store. If the user says "my notes about this repo", use project. If they say "my notes about Python in general", global.

## Common agent recipes

```bash
# Capture a short task without an editor
padz create "Fix the retry logic" --no-editor

# Capture with a body (stdin)
echo "Details go here" | padz create "Title" --no-editor

# Capture a child under an existing pad
padz create "Sub-task" --inside 1f7d95d5 --no-editor

# What's open right now?
padz list --planned --output json

# What's been done?
padz list --completed --output json

# Fuzzy title search
padz list "retry" --uuid --output json

# Filter by tag (AND logic with multiple --tag)
padz list --tag urgent --tag backend --output json

# Preview long pads without pulling full content
padz list --peek --output json

# Read one pad fully
padz view 1f7d95d5 --output json

# Get the canonical UUID for a pad you want to remember
padz uuid 1 --output json

# Mark a task done (todos mode) / done-and-delete (notes mode)
padz complete 1f7d95d5

# Tags
padz tag add 1f7d95d5 urgent
padz tag remove 1f7d95d5 urgent
padz tag list

# Hand off: bundle pads into an archive for another session
padz export 1-5 --output json

# Ingest a handoff archive
padz import /path/to/padz-<timestamp>.tar.gz
```

## Selector syntax at a glance

| Form | Meaning | Example |
|---|---|---|
| `N` | Regular DI | `3` |
| `pN` | Pinned DI | `p1` |
| `dN` | Deleted DI | `d2` |
| `arN` | Archived DI | `ar1` |
| `A.B` | Child path | `1.2` (second child of pad 1) |
| `A-B` | Range | `1-3`, `p1-p3`, `d1-d5` |
| 8+ hex chars | Short UUID | `1f7d95d5` |
| full UUID | UUID | `1f7d95d5-55c6-4dda-a688-199d91f4fd2f` |
| any other string | Title search | `"retry logic"` |

Most commands accept multiple selectors: `padz delete 1 3 p1` deletes three pads. `restore` and `purge` auto-prefix bare numbers with `d` so `padz restore 3` means `padz restore d3`.

## Not covered here

For these, run `padz <command> --help`:

- `transfer` / `clone` / `migrate` (cross-store ops; usually human-driven)
- `doctor` (data-integrity repair)
- `init` / `init-link` / shell completion (one-time setup)
- `config gen` (bootstrapping `padz.toml`)

If the user asks about any of the above, read the help output, don't guess.
