# Origin data travels as a shadow tree, not annotated values

The provenance work (`docs/proposals/provenance-and-observability.md`) must
carry each value's origin through the layered merge. We decided origins travel
in a **shadow structure merged in lockstep with the value tree** inside
`deep_merge`, rather than wrapping/annotating value nodes with origin metadata.

The deciding asymmetry: the value type is the pipeline's lingua franca —
merge, validation, defaults, serde deserialization to the typed config,
`post_validate`, listing, persistence all consume it, and almost none of them
care about origins. An annotated node type would force every one of those
either to be rewritten against a new tree or to strip annotations at each
boundary (full-tree conversions; a custom deserializer for the typed path). The
shadow keeps the value pipeline untouched: only the sites that want origins
(error construction, rendering, tracing) consult the map.

The accepted cost is a new bug class — the lockstep walk desyncing from the
value walk. Mitigations: the walk lives in one function (`deep_merge`, the
single place merge decisions happen, which is also where the provenance spec
emits win/loss trace events), locked by tests over the hostile shapes
(arrays-of-tables, quoted dotted keys, wholesale array replacement); and the
blast radius of a desync is a wrong-or-missing origin on a diagnostic, never a
corrupted resolved config — whereas the annotated design would put the
equivalent bug class inside the value pipeline itself.
