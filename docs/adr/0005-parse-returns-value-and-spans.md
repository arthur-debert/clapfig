# Parse returns the value tree and the span index together

File origins need byte spans from the same parse that produced the `Value`
tree (`docs/spec/provenance.md`). We decided `FormatAdapter::parse` returns
both — a document `{ value, spans }` — rather than a separate `span_index`
entry point over the same text.

JSON parse strips `//` comment keys; YAML parse resolves aliases. A second
walk that does not share that work indexes paths the `Value` tree does not
have, or misses paths it does. Keeping two methods and documenting "must use
the same parse" is a convention, and conventions here fail forever. A third
`parse_with_spans` sibling has the same split, just renamed.

Callers that only want a `Value` (persist, some tests) discard the index.
Computing spans on those paths is the cost of one parse; config files are
small. `Operation::SpanIndex` and `FormatAdapter::span_index` go away:
producing spans is part of parse, the way producing a `Value` is. A
successful parse's span index covers every path in the returned tree —
empty or partial indexes are not a legal degradation. How each adapter fills
the index is that adapter's business; the contract is that it does.

This is the first workstream: reshape the signatures (and restore
`PathSegment::Index` on `ConfigPath`) so later slices fill bodies against a
stable contract instead of fighting the stub.
