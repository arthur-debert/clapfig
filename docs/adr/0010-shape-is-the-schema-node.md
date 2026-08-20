# The schema node is Shape; Schema stays the object constructor

Shape algebra (`docs/spec/shape-algebra.md`) needs a first-class schema
node that is not always a named-field object. We decided that node is a
new `Shape` enum (`Leaf | Object | Map | Array | Tagged`), and
`runtime::Schema` remains the named-field object — the `Object`
constructor — rather than becoming the enum itself or being renamed to
`Object`.

`clapfig::Schema` is already the derive trait; `runtime::Schema` is
already `Schema::object(...)`. Making that struct the node would mean
"schema" is trait, object, *and* leaf-or-map-or-union. Renaming the
struct to `Object` is the same split with a public rename every runtime
example uses. `Shape` is the Spec's word for the node; keep it.

Walkers take `&Shape`. The document root is a `Shape`, but only
object-shaped constructors are legal there: `Object`, `Map`, and
`Tagged` (internally tagged objects). `Leaf` and `Array` as document
roots fail the TOML baseline; load still returns `value::Map`. This
decision does not invent a root-scalar or root-array return API.
`Clapfig::builder` takes `impl Into<Shape>` so today's object-root
calls keep passing a `Schema`. An object's field value is a `Shape` —
`Field` is not a second node type. `LeafType::Map` / `LeafType::Array`
collapse into `Shape::Map` / `Shape::Array` with a leaf item;
`Field::MapOf` / `Field::ArrayOf` are the same constructors with an
object item. Unit enums are `Shape::Leaf(Enum)`, not an object
`Schema` with `enum_variants` set and empty fields. A `Tagged`
variant is a `Schema` (the object constructor), not an arbitrary
`Shape`.

Governing spec: `docs/spec/shape-algebra.md`.
