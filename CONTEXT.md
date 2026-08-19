# Clapfig

Layered configuration for Rust applications: a schema defined in Rust resolves
values from several input types, validates them, and generates the artifacts
around them (templates, JSON Schema, docs).

## Language

**Input type**:
Where configuration values come from: files, environment variables,
programmatic overrides, URL query strings. Orthogonal to format.
_Avoid_: source (see **Origin**), layer kind

**Format**:
A serialization syntax for file inputs (TOML, YAML, JSON), implemented as a
format adapter. A format is an implementation detail of the file input type,
never the core's vocabulary.
_Avoid_: file type

**Format adapter**:
The boundary module that owns everything specific to one format: parsing into
the value model, template rendering, serialization, and any declared
capabilities (e.g. comment-preserving editing). The core never touches format
types.

**Baseline test**:
The rule that a capability enters the shared value model only if TOML
expresses it well. The baseline is never crippled or extended for another
format; format-specific deltas live in that format's adapter.

**Layer**:
Merge-order of input types on `layer_order` (`Files`, `Env`, `Url`, `Cli`).
Not where a value came from.
_Avoid_: origin, source

**Origin**:
Where a resolved value came from: input type, plus file/span/source detail
when it exists. Winner-only; travels as a shadow tree. Provenance layers
are `File`, `Env`, `Url`, `Override`, `Default` — not **Layer**
(`Default` is meaningless as a merge source; programmatic overrides are
`Cli` in merge-order and `Override` here).
_Avoid_: source (already means the retained file text in error types, and
the cause chain in Rust's `Error::source()`), source map, Layer
