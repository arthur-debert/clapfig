# Spec: provenance and observability — clapfig traces itself

## Context

Clapfig resolves a value through discovery, parsing, layered merge, defaults,
and validation. After the value-model epic (`docs/spec/value-model.md`), that
pipeline speaks clapfig `Value`, and file input goes through format adapters
(TOML, YAML, JSON). What it still does not do is retain **where** a value came
from, or narrate the work.

This Spec is the feature definition for that gap. It supersedes the accepted
proposal (`docs/proposals/provenance-and-observability.md`) as the *what and
why*. The proposal remains historical; where the two disagree, this file wins.
Two decisions already landed ahead of this work and are constraints, not
reopened questions:

- Origins key the clapfig-owned `Value` model, not a format type
  ([ADR-0001](../adr/0001-clapfig-owns-its-value-model.md)).
- Origins travel as a **shadow tree merged in lockstep with the value tree**,
  not as annotations on `Value` nodes
  ([ADR-0004](../adr/0004-origin-data-travels-as-a-shadow-tree.md)).

The adapter contract already reserves the seam
(`FormatAdapter::span_index`, `Operation::SpanIndex`, `Span`, `ConfigPath`).
The value-model audit trimmed the speculative half of that seam
(`PathSegment::Index` was removed; every shipped adapter currently *refuses*
span indexing). This epic fills the seam for real.

Glossary: **Origin** is the provenance vocabulary (where a resolved value came
from). **Input type** is where values come from (files, env, overrides, URL).
**Format** is a file serialization syntax, owned by a **format adapter**. Do
not call origins "sources" — `source` already means retained file text on
error types, and `Error::source()` in Rust.

## Problem

The pipeline is a black box with two distinct failure modes, hit by two
different people.

### Clapfig detects a problem but cannot locate it

Every *schema-driven* check that runs after the layered merge — required
fields, type checks, enum membership, shape checks — sees only the flattened
merged table. `ClapfigError::InvalidValue` and `MissingRequired` carry a
dotted key and nothing else: no file, no line, not even which input type
supplied the value. Given three config files, an env prefix, and programmatic
overrides, `invalid value for key 'database.pool_size'` leaves the user to
search every input by hand.

The only located schema-driven errors today are unknown-key errors, and their
line numbers come from `find_key_line` in `validate.rs` — a TOML text-scan
heuristic that cannot handle arrays-of-tables or inline tables, and that
silently degrades to line 0 (no snippet) for YAML and JSON. Parse errors are
already located: they carry parser spans. The crate docs already name YAML/JSON
line numbers as this epic's job.

This is the gap recorded as a qualification on #100, #101, and #102: those
features are not honestly complete while the errors they produce cannot name
their origin.

### Clapfig detects nothing, but the result is wrong

Resolution *succeeds*, no error anywhere, but the resolved config is not what
the developer believes it is. A project-file value silently loses to a
forgotten env var. A file the developer expects to be discovered is never
found. There is no error to locate — the debugging question is "what did the
merge actually do?", and clapfig cannot answer it. The current recourse is
reading clapfig's source, or forking it and adding print statements. For a
framework, that is a failed contract: the moment behavior diverges from the
user's mental model, the library must be able to narrate itself.

Both failure modes are one missing capability seen from different ends:
**clapfig does not retain or expose where values come from.**

## Goals

1. Every *schema-driven* post-merge error **on a value that exists** (type,
   enum, shape) names the winning origin of that value: layer + file + line
   when it came from a file; env var name; URL query key; override key;
   or schema default. `PostValidationFailed` (the opaque user hook) is
   excluded.
2. File-sourced location works in **every shipped format** (TOML, YAML, JSON),
   including unknown keys inside arrays-of-tables and inline tables. The
   `find_key_line` heuristic is deleted. Line/column are derived from byte
   spans at render time.
3. `MissingRequired` does **not** name a winning origin — an absent key has
   none, including when a parent map exists from some input type (no
   nearest-ancestor rule). The error enumerates the file probes (with
   outcomes) and the non-file input types consulted (env, URL when
   enabled, overrides). Under `SearchMode::FirstMatch`, candidates the
   search never reached are reported as `not probed`, never as consulted.
4. Origin identity is a structured path (`ConfigPath`), not a dotted string:
   a quoted literal key `"a.b"` is distinct from the nested path `[a] b`, and
   array entries have index segments. With `normalize_keys(true)`, provenance
   paths undergo the same key normalization as value paths, while the original
   spelling and span are retained for rendering.
5. A `trace`-level subscriber capturing a two-file + env resolution sees
   discovery probes (hits and misses), per-stage summaries at `debug`, and
   every merge override — including file-vs-file inside `Layer::Files` — with
   both candidates' origins named. Healthy resolution emits nothing at `info`
   or above. No log line at any level contains a config value.
6. The crate-level docs gain the doctrine: clapfig traces liberally; when
   behavior does not match a user's expectation, the answer is in the logs.

## Non-Goals

- A retained full candidate history (winner-only origin tree; losers go to
  `trace` and are dropped).
- A public "explain this key" API, or `config list` origin annotations.
  Both fall out of the retained tree cheaply and are follow-ons.
- A structured-diagnostics redesign of `post_validate` (it returns an opaque
  `String`; clapfig has no key path to join to the origin tree).
- A new public `Origin` type. The pipeline type stays crate-private. Existing
  public error types (`InvalidValue`, `MissingRequired`, `UnknownKeyInfo`) and
  `UnknownKeyContext` gain the facts callers already consume (file, span/line,
  env var, input type). That *is* a public-field change; it is not a new
  provenance API.
- Sensitivity metadata or opt-in raw-value logging.

## Proposed Shape

A standing design principle, alongside "struct as source of truth" and
"errors are data":

> Clapfig traces liberally. Every stage of resolution — discovery, parsing,
> layer construction, merge, validation, persistence — emits structured
> `tracing` events, so that when behavior does not match a user's
> expectation, the answer is in the logs, not in a debugger attached to a
> fork.

`tracing` is an unconditional dependency. It is effectively free when no
subscriber is installed. Making it an optional Cargo feature would remove the
narration exactly when it is needed. Level discipline: `trace` is the full
story (every candidate, every win/loss with its reason, every path probed
including misses, per-key origin assignments, defaults filled); `debug` is
per-stage summaries; `info` and above are silent during healthy resolution.
`warn` is reserved for legal-but-suspicious situations if any are identified
later; none are proposed now.

**Two contracts for values.** User-facing errors may quote the offending
value (an enum rejection naming `"rus"` is how the end user finds the typo).
Tracing events never contain config values — not at any level. Config values
routinely include tokens and passwords, and clapfig has no sensitivity
metadata, so logs carry key paths, origins, value *types*, and precedence
decisions only.

### Origin tree

Each resolution retains a shadow tree of origins, same shape as the value
tree (maps and arrays included), merged in lockstep under the same override
rules: when a value wins, its origin wins. The tree is winner-only. Arrays
replace wholesale in `deep_merge` today; the origin subtree replaces
wholesale with them.

An origin names:

- which provenance layer produced the value (`File`, `Env`, `Url`,
  `Override`, `Default`) — a new vocabulary, **not** the public `Layer` enum
  used by `layer_order`. `Layer` is merge-order (`Files` / `Env` / `Url` /
  `Cli`) and must not grow a `Default` variant. `Override` is the
  programmatic override layer (`cli_override`); clapfig receives `(key,
  value)` pairs there and cannot know whether a CLI flag, GUI field, or HTTP
  header produced them. No caller-supplied origin-label API is in this Spec.
- for files: the path, the value's byte span, and the file's full text
  (`Arc<str>`, one per parsed file, the sharing `UnknownKeyInfo` already
  uses). The parse index also carries an optional key span
  ([ADR-0006](../adr/0006-span-index-entries-are-key-and-value.md)) for
  unknown-key carets; array elements have no key token (`key: None`).
- for non-file input types: whatever they know, with file/span/text as
  `None`. Env carries the original variable name(s). URL carries the
  query-parameter key as received (dotted, percent-decoded). Override
  carries the override key. Default carries the schema key.

Schema-filled defaults (`fill_defaults_into`) and schema-shaped absences
(empty map/array materialization) carry `Default`. Coercion in `finalize`
(datetime strings, integer-for-float) does not change origin: the value's
type changes, the origin does not.

Lockstep is not only `deep_merge`. Defaults injection is a second site that
must fill origin in the same walk that fills values. Span data for a file
is produced by the same `parse` that produces the `Value` tree
([ADR-0005](../adr/0005-parse-returns-value-and-spans.md)): `parse` returns
`{ value, spans }` with each index entry `{ key: Option<Span>, value: Span }`
(ADR-0005, ADR-0006). There is no separate `span_index` walk.

### Path identity

`ConfigPath` / `PathSegment` is the shared address: span index, origin tree,
and error rendering all use it. Index segments come back — a dotted string
cannot name `plugins[3].enabled`, and the current `plugins[3]` encoding in
`schema_walk` / `lookup_value` is a display hack, not an identity. Display
of a path (dotted keys, `[n]` indexes, quoted non-bare keys) is one-way;
it is never parsed back.

### File spans

For **file** input, the format adapter's `parse` returns the path → span
index with the `Value` tree (ADR-0005). Files are the only input type
where spans exist. Every shipped adapter (TOML, YAML, JSON) locates every
path of the returned tree in its own source; a successful parse with an
empty or partial index is not a legal result. Per adapter:

- **TOML** — parse once with `toml_edit` (already a dependency, has spans)
  and convert to `Value`.
- **JSON** — a clapfig-owned walk that emits both
  ([ADR-0007](../adr/0007-json-parse-is-an-owned-walk.md)); `serde_json`
  stays for serialize and edit.
- **YAML** — `serde_norway` still builds `Value`; the same `parse` fills
  spans with `yamlpath`. A path that exists only because an alias
  expanded gets the `*name` token's span
  ([ADR-0008](../adr/0008-yaml-spans-via-yamlpath-inside-parse.md)).

Unknown-key validation stays **per-file, pre-merge**. It consults that
file's span index, not the merged origin tree (an unknown key in a
low-priority file is still that file's problem). Schema-driven post-merge
checks consult the merged origin tree (the winner).

### Discovery record

Discovery today returns only loaded files (`load_files_cached` →
`ResolveInput.files`). Missing files are dropped; under `FirstMatch`,
lower-priority directories are never visited. This epic retains every
candidate probe with its outcome: `loaded`, `missing`, `error`, or `not
probed`. That record feeds `MissingRequired` diagnostics and `trace`
discovery events. Stem-based discovery probes every enabled extension in a
directory; the record names the paths actually probed.

### Consumers, in order

1. **Errors** — the consumer this work exists to serve, and the
   #100/#101/#102 prerequisite. Renderers already draw snippets from retained
   source text; they switch from 1-indexed heuristic lines to byte spans
   (caret over the value, or over the key for unknown-key diagnostics).
   Non-file origins render as `set by environment variable …` / `set by
   URL query parameter …` / `set by a programmatic override for key …` /
   `set by schema default for key …`. `MissingRequired` renders the
   discovery record, not an origin line.
2. **Tracing** — the pipeline narrates itself per the doctrine, including
   merge losers and discovery misses. Persistence (`config set` and friends)
   emits events too; it does not grow an origin tree.
3. **Follow-ons, not in this work:** `config list` origin annotations; any
   richer explain API.

## User / Agent Stories

1. As an **end user** editing a config file, I want a post-merge validation
   error on a value I wrote (wrong enum, wrong type, wrong shape) to name
   the file and line — or the env var / URL query key / override / schema
   default that actually won — so I do not grep three files to find a typo
   I just made.
2. As an **end user** whose required key is unset, I want the error to name
   every place clapfig looked (files probed, found or missing; env prefix;
   URL query when enabled; overrides), including a nested leaf whose parent
   map exists and the case where the file I thought I wrote was never
   discovered, so I know where to add the key. No origin is claimed for
   the missing leaf.
3. As an **app developer** whose resolved config is silently wrong, I want
   `RUST_LOG=clapfig=trace cargo run` to name every merge override with both
   candidates' origins and types (never values), so a forgotten env var or
   an unexpected file win is a log line, not a fork.
4. As an **end user** of YAML or JSON (or of TOML arrays-of-tables / inline
   tables), I want unknown-key and post-merge errors to carry a correct
   line, so location is not a TOML-only courtesy.
5. As a **clapfig contributor or coding agent**, I want origin lookup to
   survive `normalize_keys(true)` and to distinguish `"a.b"` from `[a] b`,
   so diagnostics point at the node the user actually wrote.
6. As an **app developer** running a library in production, I want healthy
   resolution to be silent at `info` and I want no log line to contain a
   config value, so enabling clapfig's traces cannot leak secrets.

## Risks And Rabbit Holes

- **JSON byte offsets.** Closed by ADR-0007: an owned parse walk, not a
  new crate and not a second pass. Implementers do not improvise a
  heuristic `find_key_line` for JSON.
- **Parse/span desync.** JSON parse strips `//` comment keys; YAML parse
  resolves aliases. A second walk over the same text that does not share
  that work will index paths the `Value` tree does not have, or miss paths
  it does. The adapter contract must make "same parse" structural, not a
  convention.
- **Origin/value desync.** ADR-0004 named this bug class. A desync produces
  a wrong-or-missing origin on a diagnostic, never a corrupted resolved
  config — provided implementers do not start consulting origins on the
  value path. Hostile shapes for tests: arrays-of-tables, wholesale array
  replacement, quoted dotted keys, `normalize_keys`.
- **YAML aliases.** Closed by ADR-0008: expanded paths caret the `*name`
  token. Do not build an alias-aware origin model.
- **Overbuilding history.** A retained loser list, a public explain API, or
  `config list` annotations will look cheap once the tree exists. They are
  out of scope until a concrete consumer demands them.
- **Stem-mode probe lists.** Naming every missed extension in a directory
  (`config.toml` missing, `config.yaml` loaded) is honest; inflating
  `MissingRequired` into a dump of the format registry is not. The record
  names paths actually probed.
- **Scope creep via persistence.** Tracing `config set` is in doctrine;
  building an origin tree for edits is not.

## Cross-Cutting Concerns

- **Secrets.** Logs never contain values. Errors still may. No redaction
  framework is introduced; the safe default is the whole protection.
- **Compatibility.** `ClapfigError` is `#[non_exhaustive]`; this repo does
  not keep backwards-compat shims. Error variants and `UnknownKeyInfo` /
  `UnknownKeyContext` field changes are a hard cut with a changelog
  migration note. The origin pipeline type is crate-private, so it can
  still move after this epic.
- **Dependencies.** `tracing` is new and unconditional (ADR-0009). JSON
  spans add no crate (ADR-0007). YAML spans reuse `yamlpath` (ADR-0008).
- **Performance.** One `Arc<str>` per parsed file is the same sharing
  unknown-key rendering already pays. The origin tree is the same shape as
  the value tree; it is discarded with the resolution unless an error
  retains the facts it needs. Trace events are no-ops without a subscriber.
- **CI/release.** A capturing-subscriber test is part of the suite, not a
  manual check. Changelog fragment.

## Testing / Verification

Signature evidence, all automated:

- **Located post-merge errors on existing values** — type, enum, shape —
  name file + line for a file origin, the env var, the URL query key
  (`url` feature), the override key, or the schema default. One case per
  origin kind, not only TOML. The Default case is a runtime schema whose
  declared default fails the leaf's type or enum check.
- **Unknown keys in hostile shapes** — arrays-of-tables and inline tables
  — carry a correct line in TOML, YAML, and JSON. `find_key_line` is gone
  (no remaining callers).
- **Path identity** — `"a.b"` vs `[a] b`; `normalize_keys(true)` on
  top-level, nested, inline-table, and array-of-tables keys; the original
  spelling appears in the rendered snippet.
- **Discovery** — `MissingRequired` lists probes with outcomes and the
  non-file input types consulted; it does not name a winning origin. Under
  `FirstMatch`, unprobed lower-priority candidates are `not probed`, not
  `missing`. A nested missing leaf (parent map present from some input)
  is the same diagnostic, not a nearest-ancestor origin.
- **Tracing** — a capturing subscriber over a two-file + env resolution
  sees probes (hits and misses), `debug` stage summaries, and every
  override with both origins named. A config containing a sentinel secret
  string produces no log line containing that string, at any level. Healthy
  resolution produces no `info`/`warn`/`error` events from clapfig.
- **Lockstep** — unit tests on `deep_merge` over wholesale array
  replacement, quoted dotted keys, and nested maps: the surviving origin
  is the surviving value's origin.
- **Adapter spans** — per-format tests that a known assignment's byte
  range covers the value (and, for unknown keys, the key) in the source
  text; JSON `//` comment keys are not index entries.

Prior art to copy: `render.rs` snippet tests for unknown keys and parse
errors; `format_parity.rs` for "same mistake, same error meaning, now same
location quality"; `resolve.rs` synthetic `ResolveInput` tests (the origin
tree and probe record must be injectable the same way files already are —
no filesystem in the core walk).

## Workstream Hints

The first workstream is the contract reshape (ADR-0005, ADR-0006), not a
walking skeleton: `parse` returns `{ value, spans }` with each entry
`{ key: Option<Span>, value: Span }`, `span_index` /
`Operation::SpanIndex` go away, `PathSegment::Index` returns on
`ConfigPath`, and the origin / probe-record / error-field types land as
stubs. Existing adapters keep today's `Value` in `value` and an empty
span map so the suite stays green — an empty index is WS01 holding state,
not the finished contract (ADR-0005). Filling it is the next slice.
Several later slices build against that contract at once (adapters, merge,
post-merge errors, tracing); landing the signatures first is what keeps
them from colliding.

Vertical slices after that, not layers:

- File spans live in the three adapters, unknown-key errors consult them,
  `find_key_line` dies. Demoable as "YAML unknown key in an inline table
  prints a caret."
- Origin tree through merge + defaults, post-merge errors consult it.
  Demoable as "env beats file, `InvalidValue` names the env var."
- Discovery record into `MissingRequired`. Demoable as "unset required key
  lists the files we looked in, including misses."
- Tracing events across discovery, merge, validation, persistence, locked
  by the capturing-subscriber test.

Do not slice as "core origin logic" then "I/O" then "render."

## Out Of Scope

- `config list` origin annotations and any explain API.
- Retained merge history / loser lists in the origin tree.
- `post_validate` structured diagnostics.
- Public `Origin` type or caller-supplied origin labels on overrides.
- Raw-value logging, sensitivity metadata, redaction.
- Changing merge semantics (arrays still replace wholesale; layer order
  unchanged).
- Any format beyond TOML / YAML / JSON.

## Further Notes

- Supersedes `docs/proposals/provenance-and-observability.md` as the
  feature definition. The proposal's amendment (2026-08-17) is absorbed:
  file spans arrive through the format adapter; origin paths key clapfig
  `Value`; non-file input types keep synthetic origins.
- Prior ADRs this Spec consumes:
  - [ADR-0001 — Clapfig owns its value model](../adr/0001-clapfig-owns-its-value-model.md)
  - [ADR-0002 — Serialization formats are adapters with declared capabilities](../adr/0002-formats-are-adapters-with-declared-capabilities.md)
  - [ADR-0004 — Origin data travels as a shadow tree](../adr/0004-origin-data-travels-as-a-shadow-tree.md)
- ADRs from the grill of this Spec:
  - [ADR-0005 — Parse returns the value tree and the span index together](../adr/0005-parse-returns-value-and-spans.md)
  - [ADR-0006 — Span-index entries carry key and value ranges](../adr/0006-span-index-entries-are-key-and-value.md)
  - [ADR-0007 — JSON parse is a clapfig-owned walk](../adr/0007-json-parse-is-an-owned-walk.md)
  - [ADR-0008 — YAML spans come from yamlpath inside parse](../adr/0008-yaml-spans-via-yamlpath-inside-parse.md)
  - [ADR-0009 — tracing is an unconditional dependency](../adr/0009-tracing-is-unconditional.md)
- Sequencing: this lands **before** tagged unions (#100) and is the
  prerequisite that makes located errors for arrays-of-structs / enum
  lists (#101/#102) honest. Shape-algebra work (root-level maps #98) is
  a separate successor, not a dependency.
- Follow-on hook: once the origin tree exists, `config list` can annotate
  values with their origin at low cost. That consumer is what would justify
  promoting `Origin` to public, if anything does.
