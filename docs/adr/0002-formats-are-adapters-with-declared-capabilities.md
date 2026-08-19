# Serialization formats are adapters with declared capabilities

Following the project's existing pattern for CLI frameworks (core is agnostic,
clap is an adapter), serialization formats live behind one adapter contract:
parse text into `Value`, render a documented template from a schema, serialize
values — plus **declared capabilities** for operations not every format can
support honestly. A call site asking for an undeclared capability gets one
typed "unsupported by this format" error; no call site branches on format
names. Governing spec: `docs/spec/value-model.md` (including the baseline test:
a capability enters the shared model only if TOML expresses it well).

The capability that forced the design is comment-preserving editing
(`config set`): TOML has `toml_edit`-grade lossless editing; JSON preserves
comments trivially because they are data; YAML supports targeted span-level
patching only (see ADR-0003), with its unsupported shapes surfacing as the
typed refuse error. A blanket "all formats edit files" contract would have been
a lie; per-format silent degradation (lossy rewrites destroying user comments)
was rejected as worse than refusing.

Per-format comment representation in generated templates:

- TOML and YAML: native comments.
- JSON: the `"//"` key convention — the widely-adopted community standard
  (reserved by npm's author as never-used, canonical Stack Overflow answer) —
  with at most one `"//"` per object, an array of strings for multi-line
  prose, and suffixed keys (`"//field-name"`) for per-field docs. `"$comment"`
  was rejected: it is scoped to JSON *Schema* documents and fails
  `additionalProperties: false` validation in instances. `_comment`-style keys
  were rejected as the less-recognized variant of the same idea.

**Comment keys are format syntax, owned by the adapter.** The JSON adapter
strips every `//`-prefixed member at parse time, before the core `Value` tree
exists — exactly as TOML's `#` comments never reach the tree — so pseudo-
comments never meet schema validation, and a generated template parses and
passes default-strict validation (locked by a gen → parse → strict-validate
round-trip test). Consequences made explicit: in JSON config files the
`//`-prefixed key namespace is **reserved** — any such member is a comment,
regardless of nesting depth, and cannot be a configuration key (documented;
previously-valid literal `//…` keys are a hard cut per project policy);
duplicate comment keys are moot post-strip. The exported JSON Schema still
allowlists the pattern (`patternProperties: {"^//": {}}`) — not for clapfig's
own validation, but so third-party tooling (editors validating the instance
against the schema directly) accepts documented templates.

## Capability matrix

The authoritative per-format contract. Any unsupported cell returns the single
typed error (illustratively `UnsupportedByFormat { format, operation }`) —
call sites never branch on format names.

| Operation | TOML | JSON | YAML |
| --- | --- | --- | --- |
| Parse → `Value` + spans (ADR-0005) | yes (`toml_edit`, one parse) | yes (comment keys stripped; owned span walk, ADR-0007) | yes (aliases resolved; tags / merge keys → typed error; spans via yamlpath, ADR-0008) |
| Template generation | yes (native comments) | yes (`"//"` keys) | yes (native comments) |
| Serialize `Value` | yes | yes (non-finite floats → typed error) | yes |
| Edit: set / replace an existing value | yes, lossless (`toml_edit`) | yes (comments-as-data survive; formatting normalized, documented) | yes, targeted span patch (`yamlpatch`); byte-preserving outside the span |
| Edit: create a missing key / path | yes | yes | yes |
| Edit: create a missing file | yes (seed from generated template) | yes (seed) | yes (seed) |
| Edit: unset | yes | yes | yes |
| Known refusals | — | — | sequence-item replace; flow-style list append |

## Baseline mapping table

Both directions, per construct. "Error" always means a typed error naming the
key; adapters never silently coerce or drop.

| Construct | TOML | YAML | JSON | Owned model |
| --- | --- | --- | --- | --- |
| string / integer / float / bool / array / map | direct | direct | direct | the matching variant |
| null | n/a (inexpressible) | `null` / `~` → error: "absence expresses unset; null is not a configuration value" | `null` → same error | no null variant (baseline test) |
| non-string map key | n/a | error | n/a (always strings) | keys are `String` |
| integer outside `i64` | error | error | error | `Integer(i64)` |
| non-finite float | `inf` / `nan` native | `.inf` / `.nan` accepted | no literal (parse n/a); **serializing** a non-finite float → error | `Float(f64)` incl. non-finite |
| datetime | native, four forms | string in TOML's lexical forms + schema-driven coercion (ADR-0001) | same as YAML | `Datetime`, four forms |
| YAML anchors / aliases | n/a | resolved at parse, invisible to the model | n/a | — |
| YAML custom tags / merge keys | n/a | error (out of baseline; merge keys unsupported by the chosen parser) | n/a | — |
| JSON `//` comment keys | n/a | n/a | stripped at parse (adapter-owned) | — |

The parity suite derives an expected-result test from every non-"direct" row.
