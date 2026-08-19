# Span-index entries carry key and value ranges

The parse return (ADR-0005) includes a path → span index. We decided each
entry is `{ key: Option<Span>, value: Span }`, not a single `Span`.

Two diagnostics caret two different ranges: unknown-key errors the key
token (`kind`), post-merge value errors the assigned value (`"rus"`). A
single span makes one of those carets a lie. `key` is `None` on array
elements — there is no key token in source for `[[servers]]` entries or
JSON array items. The origin retained on the shadow tree keeps the
**value** span; unknown-key lookup uses the **key** span.

Original key spelling is not stored here. `normalize_keys` rewrites span
index paths in the same walk that rewrites the value tree; the source
text plus the key span is what the renderer shows.
