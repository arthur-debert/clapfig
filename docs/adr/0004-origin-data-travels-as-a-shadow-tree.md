# Origin data travels as a shadow tree, not annotated values

The provenance work (`docs/spec/provenance.md`) must carry each value's
origin through the layered merge. We decided origins travel in a **shadow
structure merged in lockstep with the value tree**, rather than
wrapping/annotating value nodes with origin metadata.

The deciding asymmetry: the value type is the pipeline's lingua franca —
merge, validation, defaults, serde deserialization to the typed config,
`post_validate`, listing, persistence all consume it, and almost none of
them care about origins. An annotated node type would force every one of
those either to be rewritten against a new tree or to strip annotations
at each boundary (full-tree conversions; a custom deserializer for the
typed path). The shadow keeps the value pipeline untouched: only the
sites that want origins (error construction, rendering, tracing) consult
the tree.

The accepted cost is a new bug class — the lockstep walk desyncing from
the value walk. A desync produces a wrong-or-missing origin on a
diagnostic, never a corrupted resolved config; the annotated design would
put the equivalent bug class inside the value pipeline itself.

Lockstep is not only `deep_merge`. Defaults injection (`fill_defaults_into`)
fills origin in the same walk that fills values, and file spans are
produced by the same `parse` that produces the `Value` tree (ADR-0005).
Win/loss trace events fire at `deep_merge` for every overlay win, including
file-vs-file. Tests lock the hostile shapes: arrays-of-tables, quoted
dotted keys, wholesale array replacement, `normalize_keys`.
