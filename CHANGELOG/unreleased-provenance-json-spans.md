- **JSON parse fills the span index** ([#150](https://github.com/arthur-debert/clapfig/issues/150), epic [#146](https://github.com/arthur-debert/clapfig/issues/146)) — `JsonAdapter::parse` is a clapfig-owned walk (ADR-0007) that emits the `Value` tree and the path → `{ key, value }` span index in one pass. `//`-prefixed comment keys are absent from both the tree and the index. `serde_json` stays for serialize and edit (order-preserving pretty-print, comments-as-data).

  - Every path in the returned tree has a span entry; array elements have `key: None` (ADR-0005, ADR-0006). Key spans cover the quoted JSON token (`"kind"`), value spans cover the assigned value.
  - Null and out-of-range integer/float errors now carry the offending token's byte span.
  - No span-aware JSON crate; no second walk that locates keys after a `serde_json` parse.
