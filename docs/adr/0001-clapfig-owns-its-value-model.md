# Clapfig owns its value model

Clapfig's pipeline historically trafficked in `toml::Value` — confique heritage
that survived the confique removal (#104) and miscast configuration values as a
serialization format. We decided the core speaks a clapfig-owned `Value` model,
and `toml` types exist only inside the TOML format adapter. Governing spec:
`docs/spec/value-model.md`.

Shape decisions folded into this ADR:

- **Map**: `BTreeMap<String, Value>`. The TOML baseline makes maps semantically
  unordered (#99 recorded why declaration order has no honest meaning after
  sparse multi-layer merge), so the only requirement left is deterministic
  presentation — which sorted iteration gives structurally, with no dependency.
  `IndexMap` was rejected because carrying insertion order in the core type
  would silently reintroduce what #99 ruled out.
- **Datetime**: an owned type mirroring TOML's four forms (offset date-time,
  local date-time, local date, local time), parse/display only. Re-exporting
  `toml::value::Datetime` leaks the type we are evicting; `chrono`/`time` were
  rejected because TOML's local (offset-less) forms don't map onto
  timezone-aware types and would import semantics beyond the baseline.
- **Datetime across formats**: schema-driven coercion, never sniffing. YAML and
  JSON deliver datetimes as strings (parsing is schema-blind); for leaves
  declared `DateTime`, the validation/typed pass parses the string against
  **TOML's four datetime lexical forms** — offset date-time, local date-time,
  local date, local time, i.e. the owned type's own grammar — and a string
  matching none of them is a normal type error. Each adapter serializes
  `Value::Datetime` with the same spellings (TOML natively, YAML/JSON as
  strings), so every variant round-trips in every format. "RFC 3339 only" was
  rejected: RFC 3339 defines just the offset form, which would have made three
  of the four baseline variants unreachable from YAML/JSON. Cross-adapter
  tests cover each form plus malformed input. Adapter-side "looks like a date"
  detection was rejected as implicit typing by pattern match — the Norway
  problem in another hat.
- **Serde bridge**: direct `Deserializer`/`Serializer` implementations for
  `Value` (the `serde_json::Value`/`toml::Value` pattern), with datetime
  carried via a private marker newtype the way `toml` does it. This retires the
  serialize-reparse round trip the derive path used to preserve datetimes.

## Consequences

Public surfaces that exposed `toml` types (`post_validate`'s table, schema
defaults and enum sets, the resolved output) change to the owned model — a hard
cut with a changelog migration note, per project policy. The provenance work
(`docs/proposals/provenance-and-observability.md`) keys origins to this model's
paths, which is why this refactor precedes it.
