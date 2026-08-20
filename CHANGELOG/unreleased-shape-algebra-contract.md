**Shape algebra contract** ([#165](https://github.com/arthur-debert/clapfig/issues/165), epic [#164](https://github.com/arthur-debert/clapfig/issues/164)) — the schema node is a new `Shape` enum. Hard cut, no shims (per project policy). Resolve still walks today's object schema; later slices switch walkers and implement root-map / tagged load.

- **`runtime::Shape`**: `Leaf | Object | Map | Array | Tagged`. [`runtime::Schema`](https://docs.rs/clapfig/latest/clapfig/runtime/struct.Schema.html) stays the named-field object constructor (`Schema::object(...)`), not the node and not renamed to `Object`. [`clapfig::Schema`](https://docs.rs/clapfig/latest/clapfig/trait.Schema.html) stays the derive trait. An object's field value is a `Shape`; `Field` is not a second node in this contract (the public collapse of `Field` / `LeafType::Map` / `LeafType::Array` is the pipeline-swap slice).
- **Legal document roots**: Object, Map, Tagged. Leaf and Array construct as nested shapes and panic as a document root. Root Map and Tagged construct; `Clapfig::builder` `todo!()`s them rather than loading as an object.
- **Tagged construction**: variants are objects (`Schema`). Discriminator set is closed: at least one variant; post-rename names unique and non-empty. Construction panics on empty unions, empty tag/discriminator names, collisions, and non-object variants.
- **Static mirror**: `static_schema::ShapeStatic` / `TaggedVariantStatic` so derive can emit const trees later. `Schema::shape()` defaults to wrapping `schema()` as `Shape::Object`.
- **`Clapfig::builder` takes `impl Into<Shape>`**. Object-root callers keep passing a `Schema`.

**Migration (hard cut, per project policy):**

- `Clapfig::builder(schema: Schema)` is now `Clapfig::builder(schema: impl Into<Shape>)`. Passing a `Schema` still compiles (`From<Schema> for Shape`). Function-pointer types that named `fn(Schema) -> Builder` must update.
- Exhaustive matches on schema-node types that this slice adds (`Shape`, `ShapeStatic`) must name all five constructors. `Field` / `FieldStatic` / `LeafType` are unchanged in this slice.
