# YAML spans come from yamlpath inside parse, with an alias rule

ADR-0005 requires `parse` to return the `Value` tree and the span index
together. YAML parse is `serde_norway` (ADR-0003). We decided that does
not change: `parse` still builds `Value` from norway, and **the same
`parse` call** fills the span index with `yamlpath` (already in tree for
edits). An owned YAML walker is the rabbit hole ADR-0003 refused.

The two tools disagree on aliases. Norway *expands* `db: *defaults` into
`db.host` / `db.port` in `Value`; yamlpath still sees a single `*defaults`
token. Rule: a path that exists in source gets yamlpath's key and value
ranges; a path that exists in `Value` only because an alias expanded gets
the alias reference's span (`*name`) for both `key` and `value`. One rule,
tested. Implementers do not pretend yamlpath sees the resolved tree.

The two tools stay inside the adapter. The public contract is one `parse`
call.
