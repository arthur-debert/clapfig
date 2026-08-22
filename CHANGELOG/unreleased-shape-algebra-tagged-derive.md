**Shape algebra tagged derive and artifacts** ([#169](https://github.com/arthur-debert/clapfig/issues/169), epic [#164](https://github.com/arthur-debert/clapfig/issues/164), generalizes [#100](https://github.com/arthur-debert/clapfig/issues/100)) — internally tagged `Schema` enums honor `#[serde(tag = "...")]` and export honest JSON Schema / templates. Hard cut, no shims (per project policy). Untagged / `content` / adjacent tagging stay derive-time errors naming `#[clapfig(value)]`. Flatten stays rejected.

- **Derive.** `#[serde(tag = "kind")]` two variant structs load and typed-deserialize; serde deserialize of the same tree succeeds. Unit variants are the empty object. The tag is reserved — a variant field of the tag name is a derive-time error. Discriminators follow `rename` / `rename_all`; empty unions, empty names, and post-rename collisions are derive-time errors.
- **JSON Schema.** Tagged unions export as `oneOf`; each branch is the variant object plus the tag as a required property with `{ "const": "<discriminator>" }`. No OpenAPI `discriminator`. Object-root schemas are unchanged, including the ADR-0002 `^//` comment-key allowlist. Map of tagged and nested tagged compose.
- **Templates.** `config gen` for a tagged shape emits one commented example per variant, each a complete object for that discriminator. No uncommented mixed-variant object. Object-root templates that do not use new constructors stay byte-identical.

**Migration (hard cut, per project policy):**

- `#[serde(tag = "...")]` on a `Schema` enum is now honored (it was a derive-time error). `untagged` / `content` / adjacent tagging remain errors.
- [`SchemaStatic`](https://docs.rs/clapfig/latest/clapfig/static_schema/struct.SchemaStatic.html) gains `tagged_tag` / `tagged_variants` (empty on object and unit-enum schemas).
- Internally tagged enums are legal document roots (`Clapfig::typed::<Block>()`).
