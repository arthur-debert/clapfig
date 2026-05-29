# Schema-metadata symmetry between the static and runtime paths

## Status

Planning. This document precedes implementation. It exists to capture the
design intent so review can happen before a single line of macro code is
written.

## Motivation

Clapfig has two schema entry points:

- **Static.** `Clapfig::builder::<C>()`, where `C` is a Rust struct deriving
  `confique::Config`. The schema is the struct's compile-time
  `confique::meta::Meta` tree.
- **Runtime.** `Clapfig::runtime(schema)`, where `schema` is an owned
  `clapfig::runtime::Schema` constructed at run time (see
  [runtime-schemas-and-cascading-strictness.md](runtime-schemas-and-cascading-strictness.md)).

Internally both converge on a borrowed `SchemaRef` view (`src/spec.rs`),
and most consumers — strict-mode validation, doc lookup, valid-key
enumeration, normalization, URL query, CLI overrides, layer merging,
persistence, the resolver — share one implementation across both paths.

But two fields on the shared `LeafRef` view (`src/spec.rs:213-228`) are
populated only on the runtime side:

```rust
pub(crate) struct LeafRef<'a> {
    pub default: Option<LeafDefault<'a>>,
    pub env: Option<&'a str>,
    pub optional: bool,
    pub allowed_values: Option<&'a [toml::Value]>,  // None on static
    pub ty: Option<&'a LeafType>,                   // None on static
}
```

The two `None`s on the static path cascade into four observable gaps:

1. **JSON Schema `"type"` is missing on static-path fields without a
   default.** `schema.rs:148-162` writes `"type"` from `leaf.ty`; falls
   back to inferring from the default expression. Fields without a
   default get no type key at all. Documented as a known limitation at
   `schema.rs:25-31`.
2. **JSON Schema `"enum": [...]` is never emitted on the static path.**
   `schema.rs:191-196` writes the `enum` slot only when
   `leaf.allowed_values` is `Some` — which only the runtime path ever
   populates. Confique's `Meta` does not expose the variants of a Rust
   `enum` field.
3. **`config gen` templates on the static path emit no `# Allowed: ...`
   hint for enum leaves.** Same root cause.
4. **`config gen` templates on the static path emit no type placeholder
   hint (`#port = 0`) for required leaves without a default.**
   `ops.rs:356-369` is unreachable from the static path because
   `LeafType` is never available.

These are not separate bugs. They are a single asymmetry — *the static
path's schema metadata is structurally thinner than the runtime path's*
— with four downstream consequences.

This proposal closes that asymmetry at the source by replacing the
confique-derived `Meta` with clapfig's own derive macro that emits a
full `runtime::Schema` from the struct definition. The static path then
shares not just `SchemaRef` plumbing with the runtime path, but the
exact same underlying `Schema` representation. `LeafRef::from_static`
goes away; every consumer reads from one populated structure.

## Goals and non-goals

### Goal: schema-metadata symmetry

After this change, the static and runtime paths produce identical schema
information. JSON Schema emission, template generation, persistence
validation, type checking, and strict-mode value-context lookup behave
identically regardless of which entry point built the schema.

The user-visible contract becomes one sentence: *"Static and runtime
paths produce identical schemas; the only difference is that
`Clapfig::builder::<C>().load()` returns a typed `C` while
`Clapfig::runtime(schema).load()` returns `toml::Table`."*

### Goal: keep the typed static surface

Users who derive `clapfig::Config` on a struct still get typed `load()`
output and typed `post_validate(&C)`. The macro replacing confique's
derive must produce both the schema *and* a `serde::Deserialize` impl
(or delegate the deserialize side to a generated layer struct, the way
confique does today).

### Non-goal: changing the runtime-path API

The runtime path keeps the same surface. `runtime::Schema`,
`runtime::Field`, `runtime::LeafType`, `RuntimeBuilder` — unchanged. The
work is on the static side; the runtime side is the target the static
side aligns to.

### Non-goal: eliminating every asymmetry

Three asymmetries are irreducible and stay:

- **`load()` return type:** `C` (typed) on static, `Table` on runtime.
  Direct consequence of having vs not having a compile-time `C`.
- **`post_validate` signature:** `&C` on static, `&Table` on runtime.
  Same root cause. Users who want typed access inside a runtime
  post-validate hook can `table.try_into::<MyType>()` explicitly.
- **Subschema mixing:** a static `Config` struct cannot embed a runtime
  schema, and vice versa. Documented constraint; this proposal does not
  relax it.

All three are documentable in one paragraph rather than buried as
"static path doesn't support X" footnotes across multiple surfaces.

## What the macro must enable

The macro — provisionally `#[derive(clapfig::Schema)]`, final name TBD
— is invoked on a Rust struct and emits:

1. A `clapfig::Schema` trait impl carrying a `&'static runtime::Schema`
   (or a `fn schema() -> &'static runtime::Schema`, behind a `OnceLock`).
   The exact storage shape — owned `OnceLock`, `Cow<'static, Schema>`,
   or a parallel `SchemaStatic` representation — is an open question,
   not a settled detail; see "Storage of the emitted schema" below.
2. A `serde::Deserialize` impl, either directly or via a generated
   layer struct mirroring confique's `<C as Config>::Layer` pattern.
3. (TBD per the migration plan) a `confique::Config` shim, if we want
   the static-path builder to keep accepting confique's trait bound
   during a transition window.

The remainder of this section enumerates every input the macro must
handle, with the `LeafType` / `Field` / `Schema` output it produces.

### Type mapping rules

| Rust field type                          | `LeafType` / `Field` emitted                  |
|------------------------------------------|-----------------------------------------------|
| `String`, `&'static str`                 | `LeafType::String`                            |
| `i8`/`i16`/`i32`/`i64`/`u8`/`u16`/`u32`  | `LeafType::Integer`                           |
| `u64`, `usize`, `isize`                  | `LeafType::Integer` (see TOML range note below) |
| `f32`, `f64`                             | `LeafType::Float`                             |
| `bool`                                   | `LeafType::Bool`                              |
| `toml::value::Datetime`                  | `LeafType::DateTime`                          |
| `chrono::DateTime<_>`, `chrono::NaiveDateTime` | `LeafType::DateTime` (feature-gated)    |
| `Vec<T>` where `T → LeafType`            | `LeafType::Array(Box::new(T-as-LeafType))`    |
| `BTreeMap<String, V>`, `HashMap<String, V>` where `V → LeafType` | `LeafType::Map(Box::new(V-as-LeafType))` |
| `Option<T>` where `T → LeafType`         | `T-as-LeafType`, with `optional = true`       |
| Unit-only enum `enum E { A, B, C }`      | `LeafType::Enum { values: [A, B, C] }` (variant names as strings, after `#[serde(rename_all)]`) |
| Nested struct `S: Schema`                | `Field::Nested(S::schema())`                  |
| `Vec<S>` where `S: Schema`               | `Field::ArrayOf(S::schema())`                 |
| `#[serde(untagged)] enum`                | `LeafType::Value` (see issue #47)             |
| `#[serde(tag = "...")] enum`             | `LeafType::Value` (clapfig doesn't validate variant shape) |
| Tuple / struct-variant enum (non-`untagged`) | Compile error: ask the user to opt into `LeafType::Value` explicitly or restructure |
| `toml::Value`                            | `LeafType::Value` (explicit untyped escape hatch) |
| Any other type                           | Compile error pointing at `#[clapfig(value)]` |

`String` keys on maps are non-negotiable — TOML tables are string-keyed
and clapfig's dotted-path machinery assumes it. Numeric or enum keys are
rejected.

**TOML integer range.** TOML integers are strictly signed 64-bit. A
`u64` value above `i64::MAX` (2^63 - 1) cannot be represented in TOML
at all — the failure mode is at serialize time, before the value ever
reaches a deserializer, and there is no faithful intermediate
representation. The macro emits `LeafType::Integer` for `u64` /
`usize` for symmetry with confique's current behavior, but the
emitted doc-comment must surface the limitation. Same for `isize` on
32-bit targets if we ever care, which we don't today.

**Nested-schema composition.** The `Field::Nested(S::schema())` and
`Field::ArrayOf(S::schema())` rows above are written abstractly. The
runtime `Field` enum today holds owned `Schema` values
(`src/runtime.rs:152-154`), so composing static schemas this way would
require cloning the sub-schema tree on every emit. Whether the macro
emits owned `Schema` copies, switches to `Cow<'static, Schema>`, or
relies on a parallel `SchemaStatic` type is the storage decision
flagged under "Open questions". This row in the table is a
representation-agnostic statement of intent, not a commitment to
owned values.

### Field-level attributes

The macro reads the following attributes on each field:

| Attribute                                | Effect                                       |
|------------------------------------------|----------------------------------------------|
| `///` doc comments                       | Populate `Leaf.doc` / `Schema.doc`           |
| `#[clapfig(default = expr)]`             | `Leaf.default = Some(expr.into())`           |
| `#[clapfig(env = "NAME")]`               | `Leaf.env = Some("NAME".into())`             |
| `#[clapfig(optional)]`                   | Force `optional = true` on a non-`Option<T>` field. The macro **must** reject this attribute unless the field also carries `#[clapfig(default = ...)]`, since otherwise the typed `load()` would deserialize a missing field into a `T` that has no value — runtime panic territory. The intended spelling is `Option<T>`; this attribute is the rare opt-out for "I want `optional` semantics on a non-`Option<T>` field whose default makes the type recoverable." |
| `#[clapfig(value)]`                      | Override type mapping with `LeafType::Value` |
| `#[clapfig(rename = "name")]`            | Override field name in the schema and on deserialize |
| `#[clapfig(skip)]`                       | Omit from schema and require a `Default` impl |
| `#[clapfig(allowed = [...])]`            | Override enum variant collection (e.g. for `String` fields with a known value set, equivalent to runtime's `Field::enum_of([...])`) |

The macro must also accept confique's existing `#[config(...)]` syntax
during the migration window so users don't have to update every
attribute in one PR. This is a transition convenience, not a long-term
commitment; the final state uses `#[clapfig(...)]` consistently.

### Struct-level attributes

| Attribute                                | Effect                                       |
|------------------------------------------|----------------------------------------------|
| `///` doc comments                       | Populate `Schema.doc`                        |
| `#[clapfig(name = "...")]`               | Override `Schema.name` (defaults to type name) |
| `#[clapfig(strict = bool)]`              | Set `Schema.strict` for cascading strictness |
| `#[clapfig(rename_all = "...")]`         | Apply confique/serde rename-all convention to all fields |

### Default-value expression handling

Confique today accepts a wide vocabulary in `#[config(default = ...)]`:
literals, arrays, maps, tuples. The macro must accept the same vocabulary
and produce a `toml::Value` at emit time (since `Leaf.default` is
`Option<toml::Value>`).

Concretely the macro must support:

- Scalar literals: `"localhost"`, `8080`, `8080i64`, `3.14`, `true`,
  `false`.
- Arrays: `["a", "b"]` → `toml::Value::Array(vec![...])`.
- Tables / maps: `{ a = 1, b = 2 }` syntax → `toml::Value::Table(...)`.
- Datetimes: TOML datetime literal strings parsed into
  `toml::value::Datetime`.
- A `path::to::const` reference for cases where the default is a `const
  fn` or `const` item (the macro emits a `.into()` call against the
  value, so anything `Into<toml::Value>` works).

This is roughly the union of TOML's own value grammar and Rust literal
syntax, restricted to what `toml::Value` can hold.

### Enum-variant handling

For a unit-only enum `enum Severity { Debug, Info, Warn, Error }`:

- The macro emits `LeafType::Enum { values: [String("debug"), ...] }`,
  applying the active `rename_all` convention to each variant name.
- The macro emits a `Deserialize` impl that accepts the same strings
  (this is what serde's default `Deserialize` already does for unit
  enums).
- JSON Schema then gets `"enum": ["debug", "info", "warn", "error"]`
  for free, closing observable gap #2.

For an enum that's part-tagged (variants with payloads), the macro
cannot honestly emit `LeafType::Enum` (clapfig's enum variant is
scalar-only) and emits `LeafType::Value` instead, with a compile-time
warning suggesting `#[serde(untagged)]` if the user wants the
union-shape semantics that go with `Value`.

### `LeafType::Value` interaction

The macro depends on `LeafType::Value` (issue #47) existing. Specifically:

- `#[serde(untagged)] enum` → `LeafType::Value`.
- `toml::Value` field → `LeafType::Value`.
- `#[clapfig(value)]` opt-in → `LeafType::Value`.

This is why `LeafType::Value` lands first as its own PR: the macro is
not implementable without it, and shipping it standalone unblocks the
runtime-path users (lex-fmt) who need it today.

### Trait bound on `RuntimeBuilder` vs `ClapfigBuilder`

Today:

- `Clapfig::builder::<C>()` requires `C: confique::Config`.
- `Clapfig::runtime(schema)` requires `schema: runtime::Schema`.

After this change:

- `Clapfig::builder::<C>()` requires `C: clapfig::Schema` (the new
  trait, exposing `C::schema() -> &'static runtime::Schema`).
- `Clapfig::runtime(schema)` is unchanged.

`spec::SchemaRef::from_meta` is removed; everything is `from_dynamic`.
`LeafRef.ty` and `LeafRef.allowed_values` are always populated. Their
`Option` wrappers can go away — that's the structural fix.

## Downstream surfaces, before and after

| Surface                          | Before                                       | After                                          |
|----------------------------------|----------------------------------------------|------------------------------------------------|
| JSON Schema `"type"`             | Sometimes missing on static                  | Always emitted                                 |
| JSON Schema `"enum"`             | Never emitted on static                      | Emitted from unit-enum variant names           |
| JSON Schema `"x-env"`            | Only when explicit `#[config(env)]` set      | Same — env-name derivation is still a layer concern, not a schema-time concern |
| `config gen` template            | Confique's template; no enum hints, no type placeholders | Single emitter (`ops::emit_schema`) on both paths |
| `config get`/`set` value validation | Serde round-trip on static, `LeafType::check` on runtime | `LeafType::check` on both                  |
| `post_validate` argument         | `&C` on static, `&Table` on runtime          | Unchanged (irreducible)                        |
| Strict-mode value context        | Same on both                                 | Same on both                                   |
| `Field::ArrayOf` representation  | Static path goes through `Nested` (serde implicit) | Both paths emit `ArrayOf` explicitly       |

The "After" column is the new symmetric contract. The "Before" column
is what review should weigh against — we are paying for symmetry with
the engineering cost of replacing confique's role in the static path.

## Migration plan

This is sequencing for the implementation, not part of the proposal's
design decisions. Recorded here so the planning conversation can fork
into "what should the macro do" (design) and "how do we get there"
(sequencing) cleanly.

1. **Land `LeafType::Value`** as a separate PR (small, additive, fixes
   issue #47, unblocks lex-fmt today, prerequisite for the macro's
   `untagged`-enum handling).
2. **Land the macro** behind a new derive, with both `#[derive(Config)]`
   and `#[derive(Schema)]` accepted on the static-path builder during a
   transition. New examples and tests use the new derive.
3. **Migrate the in-repo examples and `fixtures::test::TestConfig`** to
   the new derive.
4. **Deprecate `#[derive(Config)]` support** in clapfig's static-path
   builder. Confique is dropped as a dependency at this step or kept
   only as a downstream concern of users who want it for other reasons.

Each step is a separate PR. The macro PR is the design-critical one and
benefits from the planning captured here.

## Open questions

Things to settle during review of this proposal, before macro code lands:

- **Derive name.** `clapfig::Schema`? `clapfig::Config`? The latter
  reuses the confique-familiar name but creates ambiguity during the
  transition window when both crates' `Config` derives exist.
- **Storage of the emitted schema.** Three concrete obstacles to a
  `const SCHEMA: runtime::Schema = ...` form, all rooted in
  owned types inside the runtime representation:
  1. `Schema::doc` and `Leaf::doc` are `Vec<String>`. Const requires
     `&'static [&'static str]`.
  2. `Field::Nested(Schema)` and `Field::ArrayOf(Schema)` carry an
     owned `Schema` value, so composing nested schemas means cloning
     a sub-tree at every parent emit (`src/runtime.rs:152-154`). A
     `Cow<'static, Schema>` or a reference-based variant would let
     the macro emit a `&'static` pointer to a sub-type's pre-emitted
     schema instead.
  3. `Leaf.default: Option<toml::Value>` — `toml::Value` is owned and
     contains `String` / `Vec`, so it can't appear in a `const`. A
     parallel `ValueStatic` enum (`&'static str` / `&'static [...]`)
     convertible to `toml::Value` on demand would close this.

  The three options that emerge: (a) accept the allocation cost and
  use `OnceLock<runtime::Schema>`, (b) add a parallel `SchemaStatic`
  / `ValueStatic` type and have `SchemaRef` view both, (c) refactor
  the runtime types themselves to be reference-based via `Cow`. Each
  is a separate engineering investment with different ergonomic
  consequences for the runtime-path API. The decision belongs in the
  macro PR; this proposal flags the constraint.
- **Coexistence with confique during the migration.** Accept
  `C: confique::Config` *or* `C: clapfig::Schema` on the static
  builder, with a shim from the first to the second? Or hard-fork and
  require migration in one step?
- **Per-node strict declaration on static.** Once the macro is in
  place, do we add `#[clapfig(strict = false)]` as a struct attribute
  to mirror runtime's `Schema::strict`? Currently static users go
  through `builder.strict_at(path, ...)` for the same effect.
- **Stability of `LeafType` for macro consumption.** The macro emits
  `LeafType` constants. Any future widening of `LeafType` (e.g. to add
  a numeric-range constraint) becomes a macro-output churn event.
  Whether `LeafType` should be sealed against backwards-incompatible
  additions, or treated as freely extensible, is a separate decision
  this proposal doesn't take but flags.

## Out of scope

- Replacing confique's deserialize machinery with a hand-rolled one.
  The macro can generate serde-driven deserialization; we don't need
  to reinvent it.
- Schema validation of partially-known shapes (e.g. "this field accepts
  either `String` or `[String, Table]`"). That's what `LeafType::Value`
  - caller-side `post_validate` is for; we're not adding a richer union
  type to `LeafType`.
- Persistence-time round-tripping of schema metadata. The schema is
  derived from source code at compile time; we don't read it back from
  the TOML files we write.

## References

- Issue #47 — `LeafType::Value`, the prerequisite escape hatch.
- `docs/proposals/runtime-schemas-and-cascading-strictness.md` — the
  Phase 2/3 design this proposal extends.
- `src/spec.rs:213-228` — the `LeafRef` view whose two `Option` fields
  this proposal eliminates.
- `src/schema.rs:25-31` — the JSON Schema "limitation" comment whose
  underlying cause this proposal removes.
