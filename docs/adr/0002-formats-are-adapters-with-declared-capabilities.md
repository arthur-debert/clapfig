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
  prose, suffixed keys (`"//field-name"`) for per-field docs, and the exported
  JSON Schema allowlisting the pattern (`patternProperties: {"^//": {}}`) so
  documented templates validate against their own schema. `"$comment"` was
  rejected: it is scoped to JSON *Schema* documents and fails
  `additionalProperties: false` validation in instances. `_comment`-style keys
  were rejected as the less-recognized variant of the same idea.
