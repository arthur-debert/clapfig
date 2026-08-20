**Shape algebra pipeline swap** ([#166](https://github.com/arthur-debert/clapfig/issues/166), epic [#164](https://github.com/arthur-debert/clapfig/issues/164)) — walkers take [`Shape`](https://docs.rs/clapfig/latest/clapfig/runtime/enum.Shape.html). Hard cut, no shims (per project policy). Object-root load, JSON Schema, and templates are unchanged unless a schema uses a new constructor. Root-map load and tagged walk landed in later slices of this epic (see sibling fragments).

- **The node is `Shape`.** Unknown-key, defaults, finalize, strictness, JSON Schema, templates, persist, and metadata walk [`Shape`](https://docs.rs/clapfig/latest/clapfig/runtime/enum.Shape.html). An object's field value is a `Shape`; there is no second field-node enum.
- **One map constructor, one array constructor.** `LeafType::Map` / `LeafType::Array` and `Field::MapOf` / `Field::ArrayOf` / `Field::Nested` / `Field::Leaf` collapse: a map of leaves and a map of objects are [`Shape::Map`](https://docs.rs/clapfig/latest/clapfig/runtime/enum.Shape.html) with a different item (same for Array). `SchemaBuilder::map_of` / `array_of` / `nested` / `field` and `Field::map_of` / `array_of_type` still construct; they store `Shape`.
- **Unit enums stay `Shape::Leaf(Enum)`.** `Vec<UnitEnum>` / `HashMap<String, UnitEnum>` are `Shape::Array` / `Shape::Map` of that leaf.

**Migration (hard cut, per project policy):**

- `runtime::Field` is no longer a node enum (`Leaf | Nested | ArrayOf | MapOf`). It is a constructor namespace (`Field::string()`, `Field::array_of_type(...)`, …). Exhaustive matches on `Field` / `NamedField.field` must match `Shape` instead (`Leaf | Object | Map | Array | Tagged`).
- `LeafType::Map` / `LeafType::Array` are gone. Homogeneous containers are `Shape::Map` / `Shape::Array` with a leaf or object item. `Field::array_of_type` / `Field::map_of` take `impl Into<Shape>` (a `LeafType` still converts).
- `NamedField.field` is `Shape`. `FieldBuilder` builds a `Shape` (leaf, or map/array of leaves) and supports `.doc()` / `.default()` / `.optional()` / `.env()` on those constructors.
- `FieldStatic` remains the derive emission form and converts to `Shape` (enum flatten included). Exhaustive matches on a *runtime* schema tree no longer see `Field`.
