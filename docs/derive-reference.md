# Derive Reference

`#[derive(clapfig::Schema)]` is the typed path into clapfig. It reads the
struct, unit-only enum, or internally tagged enum, field types, `///`
docs, and `#[clapfig(...)]` attributes, and emits a const schema tree
that every consumer — loading, `config gen`, JSON Schema, persistence,
strictness — walks. The node those consumers walk is
[`Shape`](https://docs.rs/clapfig/latest/clapfig/runtime/enum.Shape.html)
(leaf, object, map, array, tagged union).
[`runtime::Schema`](https://docs.rs/clapfig/latest/clapfig/runtime/struct.Schema.html)
stays the named-field object constructor, not the node. Legal typed
document roots are a named-field struct, an internally tagged enum, or
`BTreeMap<String, T>` / `HashMap<String, T>` where `T: Schema`.

This page is the attribute vocabulary. Pair it with
[Getting Started](./getting-started.md) for the first load, and with the
[`Schema` derive rustdoc](https://docs.rs/clapfig) for the exact supported
and rejected type lists.

The derive does **not** generate serde `Serialize`/`Deserialize`. Derive
those yourself; clapfig uses them for the final typed deserialize.

```rust
use clapfig::Schema;
use serde::{Deserialize, Serialize};

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct AppConfig {
    /// Listen host.
    #[clapfig(default = "localhost")]
    pub host: String,

    /// Listen port.
    #[clapfig(default = 8080)]
    pub port: u16,
}
```

## Field attributes

### `default`

Compiled default — the lowest layer, always present if the key is unset
everywhere else.

```rust
#[clapfig(default = "localhost")]
host: String,

#[clapfig(default = 8080)]
port: u16,

#[clapfig(default = false)]
debug: bool,

#[clapfig(default = [])]
tags: Vec<String>,
```

Accepts string, integer, float, bool, and unary-negated numeric literals
(`-9223372036854775808i64` works for `i64::MIN`). On `Vec<T>` of scalars,
also an array literal of literals. Literals are **kind-checked** at derive
time against the field's type (per element for arrays); a default outside
the field's `allowed = [...]` set or integer `min`/`max` range is a derive
error.

Defaults are **rejected** on map-typed fields and on arrays of nested
schema types — entries are user-supplied. An absent map or array loads as
empty (`{}` / `[]`).

Enum-typed and datetime defaults have a deferred-panic contract; see
[Enums](#enums) and [Datetimes](#datetimes).

### `env`

Override the env-var name for this field. Without it, the name is derived
from the builder's prefix plus the dotted path (`MYAPP__DATABASE__URL`).

```rust
#[clapfig(env = "DATABASE_URL")]
url: String,
```

### `optional`

Force `optional = true` on a non-`Option<T>` field. Rarely needed —
`Option<T>` is the usual spelling, and omitting the field everywhere is
then valid. A non-optional scalar without a default must be provided by at
least one layer or loading fails with `MissingRequired`.

### `rename`

Override the field's schema name (the key users write in config files).

```rust
#[clapfig(rename = "pool-size")]
pool_size: usize,
```

`#[serde(rename = "name")]` alone also works — the schema follows serde's
spelling so the merged config and the typed deserialize agree. The
directional `#[serde(rename(deserialize = "..."))]` form contributes its
deserialize spelling; a serialize-only rename leaves the schema on the
Rust identifier.

If both `#[clapfig(rename)]` and `#[serde(rename)]` are present they must
match — a differing pair is a derive-time error. An explicit rename
exempts the field from a struct-level `rename_all` rule.

Rename strings are validated at derive time (non-empty, no `.` / `[` /
`]`), and two fields resolving to the same schema name are a derive error.

### `allowed`

Constrain a **scalar** leaf to a listed set. Each literal must match the
field's type; at least one value is required. Negative integer/float
literals are accepted.

```rust
#[clapfig(allowed = ["debug", "info", "warn", "error"], default = "info")]
level: String,
```

Templates emit an `Allowed:` line; JSON Schema emits `enum: [...]`. For a
closed set that is also a Rust type, prefer a unit-only enum (below) over
`allowed` on a `String`.

`allowed` is a derive error on `Vec`, nested structs, and map-of-nested
fields, and it cannot share a field with `value`.

### `min` / `max`

Tighten an integer leaf's accepted range:

```rust
#[clapfig(min = 1, max = 12, default = 4)]
workers: u8,
```

The declared bounds are intersected with the Rust integer type's own range,
so `u8` still exports `maximum: 255` unless a smaller `max` is declared.
Contradictory ranges (`min > max`, or `min = 300` on `u8`) are derive-time
errors. Defaults must fit inside the final range. JSON Schema exports the
result as `minimum` / `maximum`, and `config set` refuses out-of-range
values before writing.

`min` and `max` are only valid on integer fields, including `Option<i64>`.
They cannot be combined with `allowed` or `value`.

### `value`

Force `LeafType::Value` — a free-form leaf that accepts any value-model
shape (scalar, array, map). Reach for it when:

- The wire shape is not a clapfig field type (`PathBuf`, a newtype, a
  third-party map).
- The value can take multiple incompatible shapes (the serde
  `#[serde(untagged)]` case). Pair `value` with a shape-changing
  `#[serde(deserialize_with)]` so the schema steps aside and your
  deserializer owns the interpretation.
- A construct the derive rejects (`Vec<Vec<...>>`, map-of-map, …) still
  needs to load.

The macro does not constrain which field type `value` is applied to: you
take responsibility for the deserialize side.

Shape-**preserving** `deserialize_with` (lowercase a string, canonicalize
a path stored as a string) does **not** need `value` — the schema keeps
advertising the inferred shape and validates it before serde runs.

## Struct attributes

### `name`

Override the schema's type name (default: the Rust struct name). Used in
diagnostics and JSON Schema titles, not as a config key.

```rust
#[derive(Schema, Serialize, Deserialize)]
#[clapfig(name = "App")]
pub struct AppConfig { /* ... */ }
```

### `strict`

Set per-node strictness for this struct's subtree in the cascading
strictness system. `None` (the default) inherits; `Some(true/false)`
becomes the nearest-ancestor override for unknown keys under this node.
See the [Strictness Guide](./strictness.md).

```rust
#[derive(Schema, Serialize, Deserialize)]
#[clapfig(strict = false)]
pub struct PluginOptions { /* free-form bag */ }
```

### `rename_all`

Rewrite every field name that has no explicit `rename` through a
serde-compatible rule: `lowercase`, `UPPERCASE`, `PascalCase`,
`camelCase`, `snake_case`, `SCREAMING_SNAKE_CASE`, `kebab-case`,
`SCREAMING-KEBAB-CASE`. Conversion is serde-exact
(`serde_derive`'s `RenameRule::apply_to_field`, snake_case source),
including digit behavior (`render_2d` → `Render2d`).

**Two spellings, different jobs:**

- `#[serde(rename_all = "kebab-case")]` (or a matching clapfig/serde
  pair) converts the schema **and** serde's generated `Deserialize`, so
  typed loading agrees on the converted keys. Use this on typed structs.
- `#[clapfig(rename_all = "kebab-case")]` **alone** converts the schema
  only — the macro cannot change serde's `Deserialize`. Typed loading
  would then accept converted keys that serde fails to find. Use the
  clapfig spelling by itself on schema-only types that don't derive
  `Deserialize`.

A clapfig/serde pair naming different rules is a derive-time error. Serde's
directional `rename_all(deserialize = "...")` contributes its deserialize
rule; a serialize-only `rename_all(serialize = "...")` is accepted and
inert for config loading. Explicit field `rename`s win over the rule.

A `kebab-case` schema is incompatible with the builder's
`normalize_keys(true)` mode (which canonicalizes incoming keys to
snake_case). Pick one convention.

`name` and `strict` are **rejected** on unit-only enums: the enum flattens
to a value-level `LeafType::Enum` at every use site, which would discard
them.

## Enums

A unit-only enum deriving `Schema` becomes a constrained value set.
Out-of-set values error at load; templates document the set with
`Allowed:`; JSON Schema emits `enum: [...]`.

```rust
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    Red,
    Green,
    Yellow,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct DisplayConfig {
    #[clapfig(default = "yellow")]
    pub color: Color,
}
```

Variant names (after `rename` / `rename_all`) are the config spellings.
On variants the only supported clapfig attribute is `rename = "..."`;
anything else is a derive error. `rename_all` on the enum itself is fully
supported (serde-exact, same two-spelling rule as structs).

### Deferred-panic contract for enum-typed fields

A default on an enum-typed field (`#[clapfig(default = "letter")] mode:
Mode`) cannot be checked at derive time — the variant list lives on
another type the macro cannot see. Membership is checked at the first
`Schema::schema()` call (typically app startup). A non-variant default
**panics** naming the field and the variant list (post-rename spelling).

```rust
#[clapfig(default = "yellow")] // must be a variant of Color after rename_all
color: Color,
```

Fix the literal before shipping; this is an authoring error, not a
runtime config error.

`Option<UnitEnum>` is supported (absent → `None`). `Option<NestedStruct>`
is not: it compiles and panics at the first `schema()` call — drop the
`Option`; an absent nested section is already the empty-table state.
`Vec<UnitEnum>` is `Shape::Array` of an enum leaf;
`HashMap<String, UnitEnum>` / `BTreeMap<String, UnitEnum>` are
`Shape::Map` of an enum leaf.

Internally tagged enums (`#[serde(tag = "...")]`) are a different
constructor — see [Tagged unions](#tagged-unions). Tuple variants,
untagged / adjacent tagging, and struct-variant enums without
`#[serde(tag)]` are not a schema shape: use `#[clapfig(value)]` and take
over deserialize, or flatten to unit variants. There is no clapfig-only
`#[clapfig(tag)]`.

## Maps

String-keyed maps only — TOML map keys are strings. Homogeneous maps are
one constructor, `Shape::Map`: a map of leaves and a map of objects
differ only in the item shape.

- `HashMap<String, V>` / `BTreeMap<String, V>` where `V` is a scalar,
  `Value`, or array-of-scalar: `Shape::Map` of that leaf. Templates carry
  a `Values:` hint.
- `HashMap<String, NestedStruct>` / `BTreeMap<String, NestedStruct>`:
  `Shape::Map` of that object (TOML `[name.<key>]`).
- `HashMap<String, UnitEnum>`: `Shape::Map` of an enum leaf.

The typed **document root** may itself be `BTreeMap<String, T>` or
`HashMap<String, T>` where `T: Schema` —
`Clapfig::typed::<BTreeMap<String, T>>()` loads `[core]` / `[site]` with
no parent field. JSON Schema is `type: object` plus
`additionalProperties` of the item at the document root; `config gen`
shows a commented example entry, not an invented parent table. A
named-field struct that *contains* a map field stays the way to keep a
reserved sibling next to user-named entries.

An absent map loads as the empty map. `Option<HashMap<String, V>>` /
`Option<BTreeMap<String, V>>` keeps the presence signal (absent → `None`)
when `V` is a scalar, `Value`, or array-of-scalar. `Option` around a map
of objects — `Option<HashMap<String, NestedStruct>>`, including a
unit-enum value syntactically routed through the same Nested
classification — is a derive-time error (an absent map is already the
empty map). Non-`String` keys, map-of-map, map-of-`Option`, and map of
arrays of nested schema types are also derive-time errors.

Keys inside a map of objects (`servers.web.host` where `servers` is a
`HashMap<String, Server>`), and keys inside a root map, are not
addressable with a dotted `config set` key — the entry key is user data.
Edit the file directly.

## Arrays

Homogeneous arrays are one constructor, `Shape::Array`: a `Vec` of
leaves and a `Vec` of objects differ only in the item shape.

- `Vec<T>` of a scalar: `Shape::Array` of that leaf. A declared
  `#[clapfig(default = [...])]` wins over the empty-array materialization.
- `Vec<NestedStruct>`: `Shape::Array` of that object (TOML `[[name]]`
  array of tables). Typed loading, per-entry defaults, per-entry
  required/type checks, and strict unknown-key detection with indexed
  paths (`plugins[1].rogue`) all work. Defaults on the array itself are
  rejected.
- `Vec<UnitEnum>`: `Shape::Array` of an enum leaf so entries validate
  against the variant set.

Array is not a legal **document root** (`Vec<T>` as the typed document
type is a compile-time error). Nested use is unchanged.

Support for nested element types is **trait-resolved**: the macro emits
`<T as Schema>::STATIC` and the compiler decides whether `T` qualifies. A
non-qualifying element (`Vec<PathBuf>`) fails with the trait's
`on_unimplemented` guidance naming `#[clapfig(value)]`.

An absent array loads as the empty `Vec`. `Option<Vec<T>>` of a scalar or
`Option<Vec<UnitEnum>>` keeps the presence signal. `Option<Vec<NestedStruct>>`
has no representation (absence is already the empty array) and fails at
the first `schema()` call with drop-the-`Option` guidance.

Still rejected at derive time: `Vec<Vec<...>>`, `Vec<Option<T>>`,
`Vec<clapfig::value::Value>`, `Vec<HashMap<...>>`. Use `#[clapfig(value)]`.

Indexed dotted-key syntax (`plugins[0].id`) for `config set` is not
supported; same as maps of sections, edit the file.

## Tagged unions

Internally tagged enums honor `#[serde(tag = "...")]` — the same
attribute serde uses on the wire. There is no clapfig-only
`#[clapfig(tag)]`.

```rust
#[derive(Schema, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Block {
    #[serde(rename = "rust")]
    Rust { mount: String, params: RustParams },
    #[serde(rename = "payload")]
    Payload { mount: String, params: PayloadParams },
}
```

The tag is declared on the enum, not a field of any variant (a variant
field of the tag name is a derive-time error). Discriminators follow
`rename` / `rename_all`; empty unions, empty names, and post-rename
collisions are derive-time errors. Unit variants are the empty object:
the file carries only the tag. An internally tagged enum is a legal
document root (`Clapfig::typed::<Block>()`).

JSON Schema describes tagged alternatives as `oneOf` branches with a
`const` on the tag field. `config gen` emits one commented example per
variant, each a complete object for that discriminator — not one
uncommented object that mixes keys from several variants. Untagged /
`content` / adjacent tagging stay derive-time errors naming
`#[clapfig(value)]`. Flatten stays rejected.

## Datetimes

`clapfig::value::Datetime` is a first-class scalar (TOML's four lexical
forms: offset date-time, local date-time, local date, local time). In
YAML and JSON they are written as strings in those same spellings; a
string becomes a datetime only when the schema declares the field as one.

```rust
#[clapfig(default = "1979-05-27T07:32:00Z")]
published: clapfig::value::Datetime,
```

**Malformed-default startup panic.** Datetime defaults are *not* parsed at
derive time — the macro does not pull a datetime parser into its
dependency tree. A malformed literal (`default = "not-a-date"`) compiles
and panics with `"clapfig: invalid datetime literal in static schema
default"` the first time `Schema::schema()` is called. Verify defaults
against TOML's grammar before shipping.

Field types merely *named* `Datetime` (or `Value`) are claimed by name. A
user-defined lookalike is a compile error with guidance rather than a
silently mis-typed leaf.

## Serde attributes

Any `#[serde(...)]` attribute the schema does not honor is a **derive-time
error** naming the attribute and the divergence it would cause. There is
no silent ignore and no compatibility path.

**Honored:** `rename` (fields/variants, directional included),
`rename_all` (structs, unit-only enums, and internally tagged enums,
directional included), `#[serde(tag = "...")]` on enums (internally
tagged unions of objects), `deserialize_with` / `with` for
shape-preserving normalization. There is no clapfig-only
`#[clapfig(tag)]`.

**Accepted and inert** for config loading: serialize-only attributes
(`skip_serializing`, `serialize_with`, …) and derive plumbing (`bound`,
`borrow`, `crate`, `expecting`).

**Rejected:** `default`, `flatten`, `alias`, `skip`, `skip_deserializing`,
`untagged` / `content` (adjacent tagging), `deny_unknown_fields`,
`transparent`, `from` / `try_from`, and anything else the schema would
disagree with. Supporting any of those is future work, not a missing
flag.

A shape-changing deserializer must be paired with `#[clapfig(value)]` so
the schema declares a free-form leaf. Without `value`, schema validation
runs first and rejects the unexpected wire shape with a type error.

## Supported and rejected types

**Supported:** `String`, `bool`, integers `i8`–`i64` / `u8`–`u64` /
`usize` / `isize` (mapped to a signed 64-bit integer carrying the source
width's bounds), `f32`/`f64`, `clapfig::value::Datetime`,
`clapfig::value::Value`, `Vec<T>` (scalar or Schema-deriving element),
`HashMap<String, V>` / `BTreeMap<String, V>`, nested structs that also
derive `Schema`, unit-only enums, internally tagged enums
(`#[serde(tag = "...")]`).

**Supported `Option` wrappers:** `Option<T>` where `T` is a scalar,
`Value`, or unit-only enum; `Option<Vec<T>>` of a scalar or unit-only
enum; `Option<HashMap<String, V>>` / `Option<BTreeMap<String, V>>` where
`V` is a scalar, `Value`, or array-of-scalar. Omitting a supported
`Option` field everywhere is valid (`None`). Unqualified `Option<T>` is
not the contract — see the rejected and deferred-panic cases below.

**Rejected at derive time:** `i128`/`u128`; generic types and `where`
clauses; tuple/unit structs and non-unit enums; `Option<Option<T>>`;
`Option<HashMap<String, NestedStruct>>` / `Option<BTreeMap<String,
NestedStruct>>` (unit-enum values included — they classify as Nested at
the field site); non-`String` map keys; map-of-map / map-of-`Option` /
map of arrays of nested types; `Vec<Option<T>>` / `Vec<Vec<...>>` /
`Vec<Value>` / maps inside `Vec`; `PathBuf`, `Duration`, `char`,
newtypes, type aliases, third-party maps (the `Schema` trait's
`on_unimplemented` diagnostic names the `#[clapfig(value)]` escape
hatch); Datetime/Value lookalikes; unknown clapfig metas; `name`/`strict`
on unit-only enums; kind-mismatched or empty `allowed`; `value` +
`allowed`; invalid or contradictory integer `min`/`max`; `allowed` on
nested-struct fields; leaf attrs
(`default`/`env`/`allowed`/`min`/`max`/`optional`) on map-of-nested and
array-of-nested fields; defaults on maps and array-of-nested fields;
invalid or colliding renames; serde attributes the schema does not honor.

**Deferred panics at the first `Schema::schema()` call** (authoring
errors, not load errors — the macro cannot resolve enum-vs-struct kind
or parse datetimes at derive time):

- `Option<NestedStruct>` — drop the `Option`; an absent nested section
  is already the empty-table state.
- `Option<Vec<NestedStruct>>` — drop the `Option`; an absent array of
  nested objects is already the empty array.
- Leaf attributes (`default` / `env` / `optional`) on a struct-typed
  nested field — drop the attributes; struct fields are nested-section
  shaped. (`allowed` on a nested field is a derive-time error.)
- A default on an enum-typed field that is not a variant (post-rename
  spelling) — see [Enums](#enums).
- A malformed datetime default literal — see [Datetimes](#datetimes).

The derive rustdoc on [`Schema`](https://docs.rs/clapfig) is the
authoritative list and stays in lockstep with the implementation.
