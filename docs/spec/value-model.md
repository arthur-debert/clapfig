# Spec: the value model — clapfig speaks its own values

## Context

Clapfig is a configuration tool, not a TOML tool. It accepts configuration from
several input types — files, environment variables, programmatic overrides, URL
query strings — and files themselves can come in several serialization formats.
Historically, though, the internal lingua franca of the entire pipeline is
`toml::Value`: every layer coerces into it at construction (`env_to_table`
heuristically parses env strings into `toml::Value`; CLI/URL overrides build
`toml::Table`s), and merge, validation, defaults, schema definitions
(`Leaf.default`, `enum_of`), the resolved output, and the `post_validate`
contract all traffic in `toml` types. This is confique heritage; the confique
path itself was removed in #104 (PR #105), but its value type stayed behind as
an unexamined assumption.

Two committed pieces of direction make this the moment to fix it:

- The **provenance & observability proposal**
  (`docs/proposals/provenance-and-observability.md`, merged) builds a span index
  and origin map keyed to the value tree. Building that against `toml::Value`
  would bake the category error into a second structure.
- The project's stated ethos — *core is framework-agnostic; clap is an adapter*
  — already names the pattern. Serialization formats deserve the same treatment
  as CLI frameworks: adapters at the boundary, never the core vocabulary.

This is a **refactor, not a feature**. Its primary yield is clarity: the
codebase stops describing configuration values as TOML values, so contributors
— human and agent — stop importing format semantics into format-agnostic code.
Working YAML and JSON support is the proof the seam is real, not the motivation.

## Problem

Mistaking configuration values for TOML values has concrete costs today:

- **Boundary hacks.** JSON Schema export needs a `toml_value_to_json`
  converter; the derive path does a serialize-reparse round trip purely to
  carry `toml::Datetime` through deserialization.
- **Format lock-in by accident.** Supporting a `config.yaml` or `config.json`
  input is structurally impossible — not because anything decided against it,
  but because the pipeline's type says TOML.
- **Category mistakes.** Anyone touching format-agnostic code (merge,
  validation, defaults) sees `toml::` types and reasonably reaches for TOML
  semantics. Agents working in the codebase repeat the mistake at scale.
- **A closing window.** The provenance epic is sequenced next and keys origins
  to value-tree paths. Whatever type the tree has when that lands is the type
  every origin, span, and error field marries.

## Goals

1. A clapfig-owned value model: the core pipeline (layer construction, merge,
   defaults, validation, resolution output, schema defaults and enum sets)
   speaks clapfig `Value`, and no `toml` type appears outside the TOML adapter.
2. Serialization formats are adapters behind one contract, and **three adapters
   work end to end: TOML, YAML, JSON** — parse/load, validation with the same
   error behavior, template generation, and JSON Schema export from the owned
   model.
3. **TOML semantics are the shared baseline, enforced by the baseline test:**
   a capability enters the shared value model only if TOML expresses it well —
   the baseline is never crippled or extended to accommodate another format.
   Everything format-specific lives inside that format's adapter, and the
   expected delta is small (YAML is a superset of the baseline; JSON is close
   behind). Concretely: maps are unordered everywhere; the value vocabulary is
   TOML's (string, integer, float, bool, datetime, array, map). Formats that
   could express more do not get to; formats that express less map into the
   baseline by explicit adapter rules.
4. Obvious format gaps use the established community convention, researched
   rather than invented — JSON has no comments, so generated JSON templates
   carry documentation via the `"//"` comment-key convention (the npm-blessed,
   most widely recognized form; mechanics pinned in ADR-0002, including the
   exported JSON Schema allowlisting the key pattern so documented templates
   validate against their own schema).
5. A serde bridge: typed `C` output deserializes from `Value` directly, killing
   the datetime round-trip hack.
6. Behavior preservation for existing TOML users: resolution results and
   generated TOML artifacts are unchanged (the refactor is invisible unless you
   adopt a new format).

## Non-Goals

- Format edge-case parity or per-format expressiveness: no YAML anchors/merge
  keys/multi-document, no type tags, no ordered maps, no JSON5/JSONC dialects.
  Where formats would meaningfully diverge from the TOML baseline, we do not
  follow them.
- New configuration features. Nothing about schemas, strictness, or layering
  changes semantically.
- Provenance/source mapping — that is the next epic, which consumes this one's
  adapter seam.
- Compatibility shims or deprecation periods for the public type changes
  (project rule: hard cuts, changelog carries the migration).

## Proposed Shape

- **`value` module** — the owned model: `Value` (scalars incl. datetime, array,
  unordered map) plus the map type the public API traffics in. Deterministic
  iteration for rendering; equality and display semantics defined here, once.
- **`format` module** — the adapter contract: a format knows how to parse text
  into `Value`, render a documented template from a schema, and serialize
  values. Provenance (`docs/spec/provenance.md`, ADR-0005) folded the path→span
  index into `parse` itself — one return, not a second walk. Three
  implementations: `toml` (the existing behavior, relocated), `yaml`, `json`.
- **serde bridge** — `Deserialize`/`Serialize` between `Value` and user types,
  used by the typed load path.
- **Pipeline swap** — merge, validation walkers, defaults, env/CLI/URL layer
  construction, ops/list, and resolution output move from `toml::Value` to
  `Value`. Format-specific code (e.g. `toml_edit` comment-preserving
  persistence) stays inside its adapter.
- **Format selection and the file-name contract** — file inputs choose their
  adapter by extension, under an explicit builder contract:
  - The existing `.file_name("myapp.toml")` keeps exact-name discovery and
    enables only that extension's format — current TOML-only callers are
    unchanged.
  - New: `.file_stem("myapp")` plus an explicit enabled-formats list. Formats
    are opt-in and ordered; the default with no declaration is TOML only —
    never inferred from compiled-in cargo features. Discovery probes the stem
    across enabled extensions; more than one match **in the same directory**
    is a hard error naming both files (no silent precedence, no merging of
    same-stem siblings). Across directories, normal layering applies — each
    directory contributes at most one file.
  - Explicit paths (persist scopes, `--output`, direct file arguments) select
    their adapter by extension, independent of the enabled list.
  - The first enabled format is the app's **preferred format**: `config gen`
    with no output path renders it, and `config set` against a scope with no
    existing file creates `<stem>.<preferred extension>` seeded from the
    generated template. When exactly one same-stem file exists, `set` edits
    that file in its own format.
  Each of these cases carries an acceptance test.

## User / Agent Stories

1. As a **clapfig contributor or coding agent**, I want format-agnostic modules
   to contain no `toml::` types, so the compiler stops me from applying format
   semantics to configuration values.
2. As an **app developer**, I want to accept `config.yaml` or `config.json`
   alongside `config.toml`, so my users pick the format their ecosystem
   prefers.
3. As an **end user**, I want identical validation, strictness, and error
   behavior whatever format my file is in, so switching formats never changes
   meaning.
4. As an **app developer**, I want `config gen` to produce a documented
   template in my chosen format — including JSON, where documentation rides the
   established comment-key convention — so generated files stay self-teaching.
5. As the **provenance epic implementer**, I want every input type and format
   to hand values through one adapter interface, so source-mapping attaches at
   one seam instead of per-format branches.
6. As an **existing TOML-only user**, I want this refactor to be invisible:
   same resolution results, same generated artifacts, one changelog note about
   renamed public types.

## Risks And Rabbit Holes

- **YAML implicit typing** (the Norway problem: `no` → bool). Adapter must pin
  a strict-scalar stance consistent with the chosen crate's schema behavior;
  decided in the ADR, tested explicitly.
- **YAML crate choice.** `serde_yaml` is archived/unmaintained; successor
  selection (and its typing behavior) is a real decision — grill material, not
  a mid-implementation improvisation.
- **Comment preservation in persistence.** `config set` edits files preserving
  comments via `toml_edit` — a TOML-specific superpower. YAML/JSON equivalents
  range from trivial (JSON comment-keys are data and round-trip for free) to
  hard (YAML comment-preserving editing). The adapter contract must express
  capability differences explicitly (a format that cannot comment-preserve says
  so) instead of implying parity. Overbuilding YAML comment-preservation is the
  epic's biggest rabbit hole — do the honest capability matrix first.
- **Datetime across formats.** TOML has first-class datetimes; YAML has
  timestamps; JSON has strings. The baseline mapping table (per adapter, both
  directions) must be written down in the ADR, or each adapter will improvise.
- **Discovery ambiguity.** Two files with the same stem and different
  extensions in one search path need a defined precedence or a defined error.
- **Scope creep via "while we're at it".** The swap touches most pipeline
  files; the refactor discipline is type-swap plus hack-deletion only — no
  opportunistic behavior changes.

## Cross-Cutting Concerns

- **Compatibility:** public surface changes (`post_validate`'s map type, schema
  default/enum value types, resolved output type) are hard cuts with a
  changelog migration note, per project design principles. TOML-path behavior
  is regression-locked.
- **Observability:** none added here (that is the next epic), but the adapter
  contract is designed so the next epic adds span/source supply without
  reshaping it.
- **Dependencies:** newly introduced — `serde_norway` (YAML parsing, with its
  forked backend as a transitive dependency) and `yamlpath` + `yamlpatch`
  (YAML editing, tree-sitter transitively). Already present and re-purposed —
  `serde_json` (JSON Schema export today; additionally becomes the JSON
  adapter's parser). `toml` / `toml_edit` remain, confined to the TOML
  adapter. Selection rationale in ADR-0003.
- **CI/release:** parity suite runs in the standard gates; changelog fragment
  with migration guide.

## Testing / Verification

- **Format-parity suite** — the epic's signature evidence: one logical config
  expressed in all three formats resolves to the identical `Value` tree,
  produces identical validation errors on the same mistakes, and yields the
  same JSON Schema. Divergence-by-design cases (JSON comment keys, datetime
  mappings, YAML scalar typing) each get an explicit expected-behavior test.
- **`value` module** — pure unit tests: construction, equality, deterministic
  iteration, display.
- **serde bridge** — typed round-trips including datetime (the test that
  retires the reparse hack), `Option` fields, nested/array/map shapes.
- **Adapter contract** — per-adapter template generation goldens (TOML goldens
  unchanged from today, byte-for-byte); JSON template carries docs via the
  pinned convention; persistence capability behavior per the matrix.
- **Regression:** existing test suite passes with TOML-only usage untouched.

## Workstream Hints

Contract-first, per the project's decomposition doctrine:

- **WS01 — the contract:** `value` module, adapter trait, serde bridge; full
  types and signatures, stubbed internals where logic is pending; pure
  data-model and bridge tests. Architecture-reviewable in isolation.
- **WS02 — the swap:** pipeline moves to `Value` with the TOML adapter carrying
  existing behavior; regression suite green; hack deletions (datetime round
  trip, `toml_value_to_json`).
- **WS03a / WS03b — YAML and JSON adapters:** parallel once WS02 lands; each
  brings its parity-suite slice and template goldens.

## Out Of Scope

- Source mapping / origin data (next epic).
- Any format not TOML/YAML/JSON.
- Ordered maps or any per-format semantic extension beyond the baseline.
- Comment-preserving *editing* beyond what each format's capability entry
  honestly supports at epic close.

## Further Notes

- The merged provenance proposal
  (`docs/proposals/provenance-and-observability.md`) needs a one-paragraph
  amendment once this spec is accepted: spans/source arrive via the format
  adapter contract and origin paths key the owned `Value` model. The amendment
  rides this epic's docs PR.
- ADRs from the grill (2026-08-17):
  - [ADR-0001 — Clapfig owns its value model](../adr/0001-clapfig-owns-its-value-model.md)
    (map/datetime/coercion/serde-bridge shape).
  - [ADR-0002 — Serialization formats are adapters with declared capabilities](../adr/0002-formats-are-adapters-with-declared-capabilities.md)
    (contract, capability refusal, per-format comment representation incl. the
    JSON `"//"` convention).
  - [ADR-0003 — YAML stack: serde_norway + yamlpath/yamlpatch](../adr/0003-yaml-stack-serde-norway-and-yamlpatch.md)
    (crate selection with rejected alternatives and revisit notes).
  - [ADR-0004 — Origin data travels as a shadow tree](../adr/0004-origin-data-travels-as-a-shadow-tree.md)
    (provenance-epic decision, recorded here because it rode this docs PR).
- Discovery policy (same-directory multi-format match is a hard error) was
  resolved in the grill and folded directly into Proposed Shape above — spec
  level per the litmus, since it is user-visible.
