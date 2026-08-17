# Proposal: provenance and observability

Status: accepted. Builds on #104 (confique-path removal, landed in PR #105): one
schema model, one resolution pipeline.

Amendment (2026-08-17): the value-model refactor (`docs/spec/value-model.md`)
now precedes this work. Where this proposal says `toml_edit` supplies spans,
read: for **file inputs**, the format adapter supplies the path → span index
and source text through the adapter contract (ADR-0002) — files are the only
input type where spans exist. Non-file inputs (env, URL, overrides, defaults)
keep the synthetic origins this proposal already assigns at their layer
constructors. The uniform seam is the origin contract itself — every input
type produces `Origin` entries, with fields it cannot fill as `None` — plus
the value/origin lockstep merge (ADR-0004). Origin paths key the
clapfig-owned `Value` model (ADR-0001), not `toml::Value`. Nothing else in
this proposal changes.

Related: the source-provenance qualifications recorded on #100, #101, and #102; the
`find_key_line` heuristic and its documented limitations (`validate.rs`).

## The problem

Clapfig resolves a value through discovery, parsing, layered merge, defaults, and
validation. Today that pipeline is a black box with two distinct failure modes, hit
by two different people.

### 1. Clapfig detects a problem but cannot locate it

Every check that runs after the layered merge — required fields, type checks, enum
membership, shape checks — sees only the flattened merged table. The error it
produces carries a dotted key and nothing else: no file, no line, not even which
*layer* supplied the offending value. Given three config files, an env prefix, and
CLI overrides, `invalid value for key 'database.pool_size'` leaves the user to
search every source by hand.

The only located *schema-driven post-merge* errors today are unknown-key errors, and
their line numbers come from `find_key_line` — a text-scan heuristic that cannot
handle arrays-of-tables or inline tables and silently degrades to "line 0" (no
snippet) when it fails. (Parse errors are already located: they carry parser spans.)

This is the gap recorded as a qualification on #100, #101, and #102: those features
are not honestly complete while the errors they produce cannot name their source.

### 2. Clapfig detects nothing, but the result is wrong

The subtler and more corrosive case: resolution *succeeds*, no error anywhere, but
the resolved config is not what the developer believes it is. A project-file value
silently loses to a forgotten env var. A file the developer expects to be discovered
is never found. Layer ordering surprises. There is no error to locate — the
debugging question is "what did the merge actually *do*?", and clapfig cannot answer
it. The developer's current recourse is reading clapfig's source, or forking it and
adding print statements. For a framework, that is a failed contract: the moment
behavior diverges from the user's mental model, the library must be able to narrate
itself.

Both failure modes are one missing capability seen from different ends: **clapfig
does not retain or expose where values come from.**

## Doctrine: the framework must be narratable

This proposal adds a standing design principle, alongside "struct as source of
truth" and "errors are data":

> Clapfig traces liberally. Every stage of resolution — discovery, parsing, layer
> construction, merge, validation, persistence — emits structured `tracing` events,
> so that when behavior does not match a user's expectation, the answer is in the
> logs, not in a debugger attached to a fork.

`tracing` becomes an **unconditional dependency**. It is effectively free when no
subscriber is installed, and gating it behind a feature would remove the narration
exactly when it is needed. Level discipline:

- `trace` — the full story: every candidate per key, every win/loss decision with
  its reason (layer precedence), every path probed during discovery including
  misses, per-key origin assignments, defaults filled.
- `debug` — per-stage summaries: files discovered and parsed (with key counts),
  layers constructed, merge completed, validation passes and their outcomes.
- `info` and above — nothing during healthy resolution. A library is quiet at info.
  `warn` is reserved for legal-but-suspicious situations if any are identified
  later; none are proposed now.

**Values are never logged.** Config values routinely include tokens, passwords, and
credentials, and clapfig has no sensitivity metadata to tell them from benign
values — so the safe-by-default contract is: logs carry key paths, origins, value
*types*, and precedence decisions, never raw values, at every level. This is
sufficient for the narration stories below (the diagnostic is *which source won*,
not what it said). If raw-value logging is ever wanted for local debugging, it
requires an explicit opt-in and a sensitivity/redaction design first; neither is
part of this proposal.

## User experience

Two personas: the **developer** consuming clapfig in their application (e.g.
Edward's author), and the **end user** of that application who edits its config
files and never sees a log.

### Story 1 — end user: a located post-merge error

Maya edits `.edward/blocks.toml` and writes `kind = "rus"`. Today Edward prints:

```text
invalid value for key 'block.core.kind': value "rus" is not in allowed set: rust | docs-site | payload
```

— and Maya greps three files to find which one she touched. After this proposal,
the same error renders the way unknown-key errors already do:

```text
error: invalid value for `block.core.kind`: "rus" is not in allowed set: rust | docs-site | payload
  --> .edward/blocks.toml:12
   |
12 | kind = "rus"
   |        ^^^^^
```

When the offending value did not come from a file, the error names its actual
source instead: `set by environment variable EDWARD__BLOCK__CORE__KIND` or
`set by a programmatic override for key block.core.kind` (clapfig's override
layers receive `(key, value)` pairs and cannot see whether a CLI flag, GUI field,
or HTTP header produced them — see Origin below).

### Story 2 — end user: a missing required key names where clapfig looked

A required key is set in no layer. An absent key has no origin, so the error cannot
point at a line — instead it names the search: every file path *probed* (found or
missing) and the non-file layers consulted. "It's not set" becomes "it's not set in
`~/.config/edward/config.toml` (not found), `./.edward/blocks.toml`, or the
`EDWARD__` env" — which tells the user exactly where to add it, including the case
where the file they thought they wrote was never discovered at all.

### Story 3 — developer: the silent wrong merge

Ben sets `pool_size = 10` in his project file; the app runs with 5; nothing errors.
Today this is a fork-and-debug session. After this proposal:

```sh
RUST_LOG=clapfig=trace cargo run
```

```text
TRACE clapfig::file    probing ./myapp.toml — found
TRACE clapfig::file    probing ~/.config/myapp/config.toml — not found
DEBUG clapfig::resolve layer Files: 1 file, 14 keys
DEBUG clapfig::resolve layer Env (MYAPP__): 1 key
TRACE clapfig::merge   database.pool_size: File(./myapp.toml:8, integer) loses to Env(MYAPP__DATABASE__POOL_SIZE, integer)
DEBUG clapfig::resolve merge complete: 15 keys, 1 override across layers
```

The forgotten env var is named in one line, with both candidates' sources and the
reason the winner won — note the log names sources and types, never the values
themselves (see the no-values contract above). No code changes, no fork.

### Story 4 — developer: locating errors in shapes the heuristic can't parse

Unknown keys inside arrays-of-tables (`[[...]]`) entries and inline tables currently render
with `path:0` and no snippet. Real span data closes this: every located error gets a
correct line regardless of TOML syntax shape. This is also what makes the
diagnostics demanded by #101/#102 (arrays of structs, enum lists) — and eventually
the per-variant errors of #100 — possible at all.

### Story 5 (follow-on) — end user: "why is this value what it is?"

Once the origin map exists, `config list` can annotate every value with its source
(`database.pool_size = 5  (env: MYAPP__DATABASE__POOL_SIZE)`), giving end users the
self-serve answer without any logging. This falls out of the retained map cheaply
but is **not part of this work** — noted here so the map's shape anticipates it.

## Approach

### The origin map

A per-resolution map from key path to the winning value's origin:

```rust
struct Origin {
    layer: OriginLayer,           // File | Env | Url | Override | Default
    file: Option<PathBuf>,        // File layer only
    span: Option<Range<usize>>,   // byte range into `source`, where parser data exists
    source: Option<Arc<str>>,     // the file's full text, shared; None for non-file origins
    detail: Option<String>,       // e.g. the env var name
}
```

- **Keyed by structured paths, not dotted strings.** A dotted string is not a
  lossless node identity in TOML: a quoted literal key `"a.b"` collides with the
  nested path `[a] b`, and array entries need index segments. The map is keyed by
  `Vec<PathSegment>` with distinct `Key(String)` and `Index(usize)` segments;
  dotted/indexed strings are produced only for display, with defined escaping.
  Regression coverage includes quoted dotted keys vs. their nested lookalikes,
  inline tables, and arrays of tables.
- **`OriginLayer` is a new enum, not the public `Layer`.** The existing `Layer`
  enum is the input to `layer_order` (a merge-order vocabulary: Files/Env/Url/Cli)
  and must not grow a `Default` variant that would be meaningless as a merge
  source. `OriginLayer` is provenance vocabulary: `File`, `Env`, `Url`,
  `Override` (the programmatic override layer — clapfig receives `(key, value)`
  pairs there and guarantees only the key; it cannot know whether a CLI flag, GUI
  field, or HTTP header produced them, and no caller-supplied origin-label API is
  proposed now), and `Default` (schema-filled).
- **Spans are byte ranges plus shared source.** `(line, column)` alone cannot
  render the caret or the snippet. A file origin retains the parser's byte
  `Range<usize>` over the *value* of the assignment (unknown-key diagnostics use
  the key's range from the span index) together with the file's full source text
  in its `source: Option<Arc<str>>` field — the same sharing pattern
  `UnknownKeyInfo` uses today, so one `Arc` per parsed file serves every origin
  from that file. Non-file origins carry `span: None, source: None`. Line/column
  are derived at render time.
- **Built at parse time.** Each file parse also produces a path → span index from
  real parser span data (`toml_edit` retains spans). The `find_key_line` text
  heuristic is deleted outright — which also fixes its array-of-tables and
  inline-table blind spots. Env/URL/override layers get synthetic origins carrying
  what they know (the env var name, the override key). Defaults filled from the
  schema get `OriginLayer::Default`.
- **Normalization-aware identity.** With `normalize_keys(true)`, provenance paths
  undergo the same key normalization as value paths (a source `pool-size` span is
  reachable from the merged `pool_size` identity), while the original spelling and
  span are retained for rendering. Acceptance covers normalized top-level, nested,
  inline-table, and array-of-tables keys.
- **Carried through the merge.** `deep_merge` merges origin tables alongside value
  tables under the same override rules: when a value wins, its origin wins. The
  losing candidate's *source* is emitted to `trace` at the decision point (story 3)
  and then dropped.
- **Winner-only retention.** The retained map answers "where did the effective
  value come from". Losers live in the logs, which cost nothing to keep. A retained
  full candidate history is out of scope until a concrete consumer demands it.
- **Discovery record.** The resolution retains every candidate probe with its
  outcome — `loaded`, `missing`, or `error` — not just the files that loaded:
  missing files are exactly the useful candidates when a required key is absent
  (story 2). Under `SearchMode::FirstMatch`, candidates below the first hit are
  recorded as `not probed`, never reported as consulted. The relevant probes feed
  `MissingRequired` diagnostics.

### Consumers, in order

1. **Errors** — the load-bearing consumer and the #100/#101/#102 prerequisite.
   *Schema-driven* post-merge checks (required, type, enum, shape, future
   tagged-union variants) consult the origin map; unknown-key errors take their
   line from the span index. Error variants gain origin fields; `render` learns to
   print non-file origins. The user-supplied `post_validate` hook is **out of
   scope**: it receives the resolved config and returns an opaque `String`
   (`PostValidationFailed`), so clapfig has no key path to join to the origin map.
   A structured-diagnostics redesign of that hook (returning key paths that could
   be located) is a possible follow-on, not part of this work.
2. **Tracing** — the pipeline narrates itself per the doctrine above, including
   merge losers and discovery misses.
3. **Follow-ons, explicitly not in this work:** `config list` origin annotations;
   any richer "explain this key" API. Both fall out of the retained map; neither
   blocks it.

### What this deliberately is not

- Not a retained merge history (winner-only map; losers go to logs).
- Not a new public API surface: the origin map's type can stay crate-private,
  exposed only through error fields and rendering, until the follow-ons need more.
- Not an error-model redesign: existing variants gain origin information.
  `ClapfigError` is already `#[non_exhaustive]`; pre-1.0 SemVer and this repo's
  no-backwards-compat stance cover the field changes, with the migration note in
  the changelog.

## Sequencing

This lands **before** the shape-algebra work (root-level maps #98, derive `Vec`
bridge #101/#102) and is a hard prerequisite for tagged unions (#100), whose branch
selection can only happen post-merge and would otherwise produce exactly the
located-nowhere errors this proposal eliminates.

## Acceptance

- Every *schema-driven* post-merge error (required, type, enum, shape) names
  layer + file + line when the value came from a file; the env var name for
  env-sourced values; and the override key for programmatic overrides.
  `PostValidationFailed` (the opaque user hook) is explicitly excluded.
- Missing-required errors enumerate the file probes (with outcomes) and layers
  consulted; under `FirstMatch`, unprobed candidates are not reported as consulted.
- `find_key_line` is gone; unknown-key errors inside arrays-of-tables and inline
  tables carry correct lines (regression tests on both shapes), and origin lookups
  survive `normalize_keys(true)` and quoted dotted keys (tests distinguishing
  `"a.b"` from `[a] b`).
- A `trace`-level subscriber capturing a two-file + env resolution sees discovery
  probes (hits and misses), per-layer summaries, and every cross-layer override
  with both candidates' sources named (locked by a capturing-subscriber test).
- No log line at any level contains a config value (asserted by the same
  capturing-subscriber test over a config containing a sentinel secret string).
- Healthy resolution emits nothing at `info` or above.
- The doctrine paragraph lands in the crate-level docs' design principles.
