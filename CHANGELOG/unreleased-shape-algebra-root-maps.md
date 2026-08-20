**Shape algebra root maps** ([#167](https://github.com/arthur-debert/clapfig/issues/167), epic [#164](https://github.com/arthur-debert/clapfig/issues/164), generalizes [#98](https://github.com/arthur-debert/clapfig/issues/98)) — a schema whose document root is a homogeneous Map loads, validates, and generates artifacts with no synthetic parent field. Hard cut, no shims (per project policy). Tagged walk landed in SHP01-WS04.

- **Runtime.** `Clapfig::builder(Shape::map(...))` loads `[core]` / `[site]` as map entries. Unknown keys, missing required fields, and type errors name origin the same way a named Map field already did.
- **Typed.** `Clapfig::typed::<BTreeMap<String, T>>()` / `HashMap<String, T>` where `T: Schema` is the twin. Legal document roots on the typed path are named-field structs (and those maps); a scalar or `Vec<T>` as the document type is a compile-time error (`DocumentRoot`).
- **JSON Schema.** Root map is `type: object` plus `additionalProperties` of the item at the document root. Object-root export is unchanged.
- **Templates.** `config gen` shows a commented example entry (`[<key>]`), not an invented parent table. Object-root templates stay unchanged.
- **Persistence.** `config set` of a dynamic entry key refuses with [`UnaddressableKey`](https://docs.rs/clapfig/latest/clapfig/enum.ClapfigError.html) — the same class of error a named Map field already used. This epic does not invent addressing for dynamic keys.

**Migration (hard cut, per project policy):**

- `Clapfig::typed::<C>()` is now generic over `DocumentRoot` (a named-field `Schema` struct, or `BTreeMap`/`HashMap<String, T>` where `T: Schema`). Unit-only enums remain valid *field* types and still derive `Schema`; they are not legal document roots.
- `FormatAdapter::template` takes `&Shape`. Object-root callers wrap `Shape::Object(schema)` (or pass the shape the builder already holds).
- `json_schema::generate_from_shape` is the shape-node exporter; `generate_schema(&Schema)` remains the object-root convenience.
