# YAML stack: serde_norway for parsing, yamlpath/yamlpatch for editing

`serde_yaml` is archived (2024). For parsing we chose **`serde_norway`**: a
maintained conservative fork (maintainer also owns the forked backend), one of
the two successors the RustSec advisory for `serde_yml` recommends, drop-in
untyped-`Value` parsing matching clapfig's schema-blind parse-then-validate
pipeline, and source-verified strict scalars (only `true/false` spellings are
booleans — no Norway problem; the `serde_yaml` family always had YAML-1.2-core
behavior here despite its 1.1 reputation).

For comment-preserving editing we chose **`yamlpath` + `yamlpatch`** (zizmor
project): tree-sitter span mapping plus span-level surgery, byte-preserving
outside the edited span, production-exercised by zizmor's auto-fix, released on
its active train. Deliberately scoped to targeted single-value patches — which
is exactly what `config set` is. Its documented gaps (sequence-item replace,
flow-style list append) surface through the capability contract (ADR-0002) as
typed refuse errors, not silent degradation.

## Considered options

- `serde_yml` — disqualified: RUSTSEC-2025-0068 (unsound, archived,
  AI-generated fork).
- `serde_yaml_ng` — trustworthy but dormant (no release since 2024-05;
  maintainer describes it as a personal library).
- `serde-saphyr` — most actively developed, pure Rust, but its types-as-schema
  design mismatches clapfig's untyped parse-to-`Value` stage, and it has a
  single pseudonymous maintainer.
- `yaml-rust2` / `saphyr` — healthy parsers, no shipped serde layer.
- `yaml-edit` (rowan CST, the architecturally ideal lossless editor) —
  credible maintainer, right design (taplo proves rowan works for config
  formats), but v0.2.x with near-zero adoption and an independent field test
  showing lost comments and corrupted indentation. **Revisit when it matures**;
  nothing in any language currently offers `toml_edit`-grade YAML guarantees
  (even `yq` is comment-carrying, not byte-preserving).
