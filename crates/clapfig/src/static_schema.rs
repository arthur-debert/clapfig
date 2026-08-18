//! Static-form, const-friendly schema types — the emission target of the
//! `#[derive(clapfig::Schema)]` proc macro.
//!
//! The runtime-side [`Schema`](crate::runtime::Schema) holds owned
//! `String` / `Vec` / [`Value`](crate::value::Value) data. That shape is convenient for a
//! builder-built schema but unfit for a `const SCHEMA: ... = ...` form, so
//! the macro emits a parallel tree whose every field is `&'static`. At
//! first call, [`Schema::schema`] caches a converted
//! `runtime::Schema` in a per-type `OnceLock`; all existing schema
//! consumers walk the cached runtime view.
//!
//! This file is the single source of truth for that mirror.

use std::sync::{Arc, OnceLock};

use crate::runtime::{
    Field as RuntimeField, Leaf as RuntimeLeaf, LeafType as RuntimeLeafType,
    NamedField as RuntimeNamedField, Schema as RuntimeSchema,
};
use crate::value::Value;

/// `const`-friendly mirror of [`runtime::Schema`](crate::runtime::Schema).
///
/// The macro emits one of these per struct (and one per nested struct).
/// Convert to the runtime form via [`SchemaStatic::to_runtime`] or read
/// the cached runtime view via [`Schema::schema`].
///
/// Unit-only enums also derive [`Schema`] (so a field of that type can
/// compose via the same `<T as Schema>::STATIC` reference the macro uses
/// for nested structs). For an enum the `fields` slice is empty and
/// `enum_variants` carries the variant names (post-`rename_all` /
/// per-variant `rename`). The converter inspects `enum_variants` when
/// flattening a `FieldStatic::Nested { .. }` (or `MapOf { .. }`): a
/// non-empty list becomes a `Field::Leaf` with `LeafType::Enum` (for
/// `MapOf`, `LeafType::Map(Enum)`; for `ArrayOf`, `LeafType::Array(Enum)`),
/// while an empty list keeps the nested-object shape.
#[derive(Debug)]
pub struct SchemaStatic {
    pub name: &'static str,
    pub doc: &'static [&'static str],
    pub strict: Option<bool>,
    pub fields: &'static [NamedFieldStatic],
    /// For unit-only enum types: variant names (post-rename). For struct
    /// schemas this slice is empty.
    pub enum_variants: &'static [&'static str],
}

/// `const`-friendly mirror of [`runtime::NamedField`](crate::runtime::NamedField).
#[derive(Debug)]
pub struct NamedFieldStatic {
    pub name: &'static str,
    pub field: FieldStatic,
}

/// `const`-friendly mirror of [`runtime::Field`](crate::runtime::Field).
///
/// `Nested`, `ArrayOf`, and `MapOf` carry the *field-site* doc comment
/// alongside the referenced schema: the type-level doc describes the type
/// in general, while the `///` lines written at the field describe this
/// particular usage. The converter prefers the field-site doc when it is
/// non-empty (mirroring how `Option<UnitEnum>` leaves keep their field
/// doc) and falls back to the referenced schema's own doc otherwise.
#[derive(Debug)]
pub enum FieldStatic {
    Leaf(LeafStatic),
    Nested {
        schema: &'static SchemaStatic,
        /// Field-site `///` doc lines (may be empty).
        doc: &'static [&'static str],
    },
    /// Arrays of nested objects (TOML `[[name]]`). Emitted by the derive
    /// macro for `Vec<T>` fields where `T` derives [`Schema`] and is not
    /// a scalar. When the item type is a unit-only enum the converter
    /// flattens this to a `Leaf` of `Array(Enum { .. })` — the
    /// array-shaped sibling of the `MapOf` enum flatten.
    ArrayOf {
        schema: &'static SchemaStatic,
        /// Field-site `///` doc lines (may be empty).
        doc: &'static [&'static str],
    },
    /// Maps of nested objects (TOML `[name.<key>]`). Emitted by the
    /// derive macro for `HashMap<String, NestedStruct>` /
    /// `BTreeMap<String, NestedStruct>` fields where the value type
    /// derives [`Schema`]. When the value type is a unit-only enum the
    /// converter flattens this to a `Leaf` of `Map(Enum { .. })` — the
    /// map-shaped sibling of the `Nested` enum flatten.
    MapOf {
        schema: &'static SchemaStatic,
        /// Field-site `///` doc lines (may be empty).
        doc: &'static [&'static str],
    },
}

/// `const`-friendly mirror of [`runtime::Leaf`](crate::runtime::Leaf).
#[derive(Debug)]
pub struct LeafStatic {
    pub doc: &'static [&'static str],
    pub ty: LeafTypeStatic,
    pub default: Option<ValueStatic>,
    pub optional: bool,
    pub env: Option<&'static str>,
}

/// `const`-friendly mirror of [`runtime::LeafType`](crate::runtime::LeafType).
#[derive(Debug)]
pub enum LeafTypeStatic {
    String,
    /// Signed 64-bit integer (TOML's only integer width), carrying the
    /// source Rust type's range so out-of-range values fail the schema
    /// check with the key path instead of a post-merge deserialize
    /// failure.
    ///
    /// The derive macro maps every Rust integer type, including the
    /// unsigned ones (`u8`/`u16`/`u32`/`u64`/`usize`) and `isize`, to
    /// this variant, emitting the width's bounds (`u8` → `0..=255`).
    /// `i64` is unbounded. `isize` carries `isize::MIN/MAX as i64` so a
    /// 32-bit target rejects values the `i64` value model can hold but
    /// `isize` cannot (on 64-bit those bounds equal the value-model
    /// range). `u64` carries `min: Some(0)` with an open upper end —
    /// values that exceed `i64::MAX` (e.g. a `u64` holding 2^63)
    /// **cannot be represented in TOML at all** — the failure mode is
    /// at serialize time, before the value ever reaches a deserializer,
    /// and there is no faithful intermediate. `usize` is the same on
    /// 64-bit (`usize::MAX as i64` would wrap to -1); on narrower
    /// targets it emits `Some(usize::MAX as i64)`. Field types like
    /// `u64` are accepted because they are convenient and round-trip
    /// correctly for the overwhelming majority of values; callers who
    /// need the full unsigned-64 range should store them as `String`
    /// and parse explicitly.
    ///
    /// `i128` and `u128` are rejected at derive time with a compile
    /// error rather than silently truncated.
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float,
    Bool,
    DateTime,
    Array(&'static LeafTypeStatic),
    Map(&'static LeafTypeStatic),
    Enum {
        values: &'static [ValueStatic],
    },
    /// Defer the enum variant set to a referenced `SchemaStatic`.
    ///
    /// Emitted by the derive macro in two scenarios that the runtime
    /// representation can't tell apart syntactically:
    ///
    /// - **Leaf attrs on a nested-typed field** —
    ///   `#[clapfig(default = "letter")] page_size: PdfPageSize`. The
    ///   macro sees `Nested` and the leaf attrs together and routes
    ///   through `EnumRef`.
    /// - **`Option<Nested>` wrapping** — `page_size: Option<Mode>` (or
    ///   `Option<DbStruct>`). The macro can't tell `Mode` (unit enum)
    ///   from `DbStruct` (struct) at the field site; both classify as
    ///   `Optional(Nested(_))`. Routing through `EnumRef` lets
    ///   `Option<UnitEnum>` work; `Option<NestedStruct>` falls through
    ///   to the deferred-kind check.
    ///
    /// `field_name` is the parent struct's field name (post any
    /// `#[clapfig(rename = ...)]`); the converter uses it inside the
    /// authoring-error panic message so the user can locate the
    /// offending field without grepping. Same deferred-error pattern
    /// as the datetime-default literal parsing.
    EnumRef {
        schema: &'static SchemaStatic,
        field_name: &'static str,
    },
    Value,
}

/// `const`-friendly mirror of [`Value`] for default-value emission.
///
/// Datetimes are stored as their string form and parsed on conversion,
/// since the owned [`Datetime`](crate::value::Datetime) is not
/// `const`-constructible.
#[derive(Debug)]
pub enum ValueStatic {
    String(&'static str),
    Integer(i64),
    Float(f64),
    Bool(bool),
    Datetime(&'static str),
    Array(&'static [ValueStatic]),
    Table(&'static [(&'static str, ValueStatic)]),
}

impl SchemaStatic {
    pub fn to_runtime(&self) -> RuntimeSchema {
        RuntimeSchema {
            name: self.name.to_string(),
            doc: self.doc.iter().map(|s| (*s).to_string()).collect(),
            strict: self.strict,
            fields: self
                .fields
                .iter()
                .map(NamedFieldStatic::to_runtime)
                .collect(),
        }
    }

    /// `true` when this schema represents a unit-only enum rather than a
    /// struct. The macro emits an empty `fields` slice for enums and
    /// populates `enum_variants` instead; the converter consults this
    /// when flattening a `FieldStatic::Nested(...)` into the runtime form.
    pub fn is_enum(&self) -> bool {
        !self.enum_variants.is_empty()
    }
}

impl NamedFieldStatic {
    fn to_runtime(&self) -> RuntimeNamedField {
        RuntimeNamedField {
            name: self.name.to_string(),
            field: self.field.to_runtime(),
        }
    }
}

/// Pick the doc lines a converted nested/map/enum field should carry:
/// the field-site `///` doc when present (it describes this usage),
/// otherwise the referenced type's own doc.
fn effective_doc(field_doc: &[&str], type_doc: &[&str]) -> Vec<String> {
    let chosen = if field_doc.is_empty() {
        type_doc
    } else {
        field_doc
    };
    chosen.iter().map(|d| (*d).to_string()).collect()
}

/// Runtime `Value` list for a unit-only enum schema's variant names.
fn enum_variant_values(s: &SchemaStatic) -> Vec<Value> {
    s.enum_variants
        .iter()
        .map(|v| Value::String((*v).to_string()))
        .collect()
}

impl FieldStatic {
    fn to_runtime(&self) -> RuntimeField {
        match self {
            FieldStatic::Leaf(leaf) => RuntimeField::Leaf(leaf.to_runtime()),
            // Flatten an enum-kind nested schema (a unit-only enum that
            // derived `Schema`) into a runtime leaf carrying the variant
            // list. The macro can't tell at parse time whether a field's
            // type is a struct or an enum — so it always emits
            // `FieldStatic::Nested { .. }`, and the kind distinction
            // happens here.
            FieldStatic::Nested { schema: s, doc } if s.is_enum() => {
                RuntimeField::Leaf(RuntimeLeaf {
                    doc: effective_doc(doc, s.doc),
                    ty: RuntimeLeafType::Enum {
                        values: enum_variant_values(s),
                    },
                    default: None,
                    optional: false,
                    env: None,
                })
            }
            FieldStatic::Nested { schema: s, doc } => {
                let mut runtime = s.to_runtime();
                runtime.doc = effective_doc(doc, s.doc);
                RuntimeField::Nested(runtime)
            }
            // Array-shaped sibling of the `MapOf` enum flatten below: a
            // `Vec<UnitEnum>` field must surface as `Array(Enum { .. })` —
            // keeping the `ArrayOf` object shape here would emit a schema
            // of zero fields that rejects every string entry at load
            // ("expected map, got string").
            FieldStatic::ArrayOf { schema: s, doc } if s.is_enum() => {
                RuntimeField::Leaf(RuntimeLeaf {
                    doc: effective_doc(doc, s.doc),
                    ty: RuntimeLeafType::Array(Box::new(RuntimeLeafType::Enum {
                        values: enum_variant_values(s),
                    })),
                    default: None,
                    optional: false,
                    env: None,
                })
            }
            FieldStatic::ArrayOf { schema: s, doc } => {
                let mut runtime = s.to_runtime();
                runtime.doc = effective_doc(doc, s.doc);
                RuntimeField::ArrayOf(runtime)
            }
            // Map-shaped sibling of the `Nested` enum flatten: a
            // `HashMap<String, UnitEnum>` field must surface as
            // `Map(Enum { .. })` — keeping the `MapOf` object shape here
            // would emit a schema of zero fields that rejects every
            // string entry at load ("expected map, got string").
            //
            // `MapOf` absence semantics survive the flatten: an absent map
            // is the empty map (entries are user-supplied), so
            // `fill_defaults` materializes `{}` for non-optional map
            // leaves and the typed deserialize yields an empty `HashMap`
            // instead of a missing-required error.
            FieldStatic::MapOf { schema: s, doc } if s.is_enum() => {
                RuntimeField::Leaf(RuntimeLeaf {
                    doc: effective_doc(doc, s.doc),
                    ty: RuntimeLeafType::Map(Box::new(RuntimeLeafType::Enum {
                        values: enum_variant_values(s),
                    })),
                    default: None,
                    optional: false,
                    env: None,
                })
            }
            FieldStatic::MapOf { schema: s, doc } => {
                let mut runtime = s.to_runtime();
                runtime.doc = effective_doc(doc, s.doc);
                RuntimeField::MapOf(runtime)
            }
        }
    }
}

impl LeafStatic {
    fn to_runtime(&self) -> RuntimeLeaf {
        // EnumRef defaults name a variant of a *different* type, so the
        // derive macro cannot check membership at expansion time (it only
        // sees `<T as Schema>::STATIC` syntactically). Check here, at the
        // first `schema()` call — the same deferred-authoring-error
        // pattern as the struct-vs-enum EnumRef assert below.
        // Guarded on `is_enum()`: a struct-typed misuse of EnumRef must
        // reach `LeafTypeStatic::to_runtime`'s struct-vs-enum assert and
        // get that diagnostic, not a variant-membership one.
        if let LeafTypeStatic::EnumRef { schema, field_name } = &self.ty
            && schema.is_enum()
            && let Some(ValueStatic::String(default)) = &self.default
        {
            assert!(
                schema.enum_variants.contains(default),
                "clapfig: field `{field_name}` has default {default:?}, which is not a \
                 variant of enum `{}` (variants: {:?}). Fix the \
                 `#[clapfig(default = ...)]` literal to name an existing variant \
                 (post-rename spelling).",
                schema.name,
                schema.enum_variants,
            );
        }
        RuntimeLeaf {
            doc: self.doc.iter().map(|s| (*s).to_string()).collect(),
            ty: self.ty.to_runtime(),
            default: self.default.as_ref().map(ValueStatic::to_value),
            optional: self.optional,
            env: self.env.map(|s| s.to_string()),
        }
    }
}

impl LeafTypeStatic {
    pub fn to_runtime(&self) -> RuntimeLeafType {
        match self {
            LeafTypeStatic::String => RuntimeLeafType::String,
            LeafTypeStatic::Integer { min, max } => RuntimeLeafType::Integer {
                min: *min,
                max: *max,
            },
            LeafTypeStatic::Float => RuntimeLeafType::Float,
            LeafTypeStatic::Bool => RuntimeLeafType::Bool,
            LeafTypeStatic::DateTime => RuntimeLeafType::DateTime,
            LeafTypeStatic::Array(elem) => {
                // Deferred enum-kind check for `Option<Vec<T>>` where `T`
                // is a nested schema type. The macro can't syntactically
                // tell a unit enum from a struct, so it emits
                // `Array(EnumRef)` for both; only the enum kind has a
                // representation (an optional array-of-enum leaf). The
                // struct kind lands here — before the generic `EnumRef`
                // conversion below, whose remediation text (drop attrs /
                // drop `Option<T>`) would mislead for the `Vec` wrapping.
                if let LeafTypeStatic::EnumRef { schema, field_name } = elem {
                    assert!(
                        schema.is_enum(),
                        "clapfig: field `{field_name}` is an `Option<Vec<{schema_name}>>` where \
                         `{schema_name}` is a struct, not a unit-only enum. An absent array of \
                         nested objects is already the empty array, so the `Option` wrapper adds \
                         no signal and has no schema representation — drop it and use \
                         `Vec<{schema_name}>`.",
                        schema_name = schema.name,
                    );
                }
                RuntimeLeafType::Array(Box::new(elem.to_runtime()))
            }
            LeafTypeStatic::Map(v) => RuntimeLeafType::Map(Box::new(v.to_runtime())),
            LeafTypeStatic::Enum { values } => RuntimeLeafType::Enum {
                values: values.iter().map(ValueStatic::to_value).collect(),
            },
            LeafTypeStatic::EnumRef { schema, field_name } => {
                // Deferred enum-kind check. The macro can't syntactically
                // distinguish a unit enum from a struct at the field
                // site, so two distinct authoring paths land here and
                // need separately-named remediations:
                //
                //   1. `#[clapfig(default = ...)] field: SomeStruct` —
                //      leaf attrs on a nested struct. Drop the attrs
                //      (struct fields are nested-section shaped).
                //   2. `field: Option<SomeStruct>` — `Option`-wrapped
                //      nested struct. Drop the `Option` wrapper (an
                //      absent nested section is already the empty-
                //      table state).
                //
                // Same first-`schema()`-call failure mode as a
                // malformed datetime default.
                assert!(
                    schema.is_enum(),
                    "clapfig: field `{field_name}` references type `{schema_name}` which is a \
                     struct, not a unit-only enum. The derive macro routed this field through \
                     `LeafTypeStatic::EnumRef` because either (a) it carries leaf attributes \
                     (`default` / `env` / `optional`) — drop the attributes; struct fields are \
                     nested-section shaped — or (b) the type is `Option<{schema_name}>` — drop \
                     the `Option` wrapper; an absent nested section is already the empty-table \
                     state. If `{schema_name}` is meant to be a unit-only enum, change its body \
                     to `enum {schema_name} {{ ... }}` with payload-free variants.",
                    schema_name = schema.name,
                );
                RuntimeLeafType::Enum {
                    values: schema
                        .enum_variants
                        .iter()
                        .map(|v| Value::String((*v).to_string()))
                        .collect(),
                }
            }
            LeafTypeStatic::Value => RuntimeLeafType::Value,
        }
    }
}

impl ValueStatic {
    pub fn to_value(&self) -> Value {
        match self {
            ValueStatic::String(s) => Value::String((*s).to_string()),
            ValueStatic::Integer(i) => Value::Integer(*i),
            ValueStatic::Float(f) => Value::Float(*f),
            ValueStatic::Bool(b) => Value::Boolean(*b),
            ValueStatic::Datetime(s) => Value::Datetime(
                s.parse()
                    .expect("clapfig: invalid datetime literal in static schema default"),
            ),
            ValueStatic::Array(items) => {
                Value::Array(items.iter().map(ValueStatic::to_value).collect())
            }
            ValueStatic::Table(entries) => {
                let mut t = crate::value::Map::new();
                for (k, v) in entries.iter() {
                    t.insert((*k).to_string(), v.to_value());
                }
                Value::Map(t)
            }
        }
    }
}

/// Marker trait implemented by structs deriving [`clapfig::Schema`](crate::Schema).
///
/// The macro emits a [`STATIC`](Schema::STATIC) associated const carrying
/// the const-form schema tree, plus a [`schema`](Schema::schema) accessor
/// that lazily converts and caches a runtime
/// [`Schema`](crate::runtime::Schema). The associated const lets nested
/// struct references (e.g. `<DbConfig as Schema>::STATIC`) appear inside
/// the parent's `static SchemaStatic = ...` initializer — fn-form trait
/// methods cannot, since trait fns are not callable in const contexts on
/// stable Rust.
///
/// Every existing schema consumer (JSON-Schema emission, template
/// generation, persistence validation, strictness cascade, etc.) walks
/// the cached runtime view, so static and runtime entry points produce
/// byte-identical behavior.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a field type `#[derive(clapfig::Schema)]` supports",
    label = "no schema shape for this type",
    note = "supported scalars: String, bool, integers (i8–i64, u8–u64, usize, isize), f32/f64, \
            clapfig::value::Datetime, clapfig::value::Value; wrappers: Option<T>, Vec<T> (scalar \
            or Schema-deriving element), HashMap/BTreeMap<String, V>",
    note = "other types (PathBuf, Duration, char, newtypes, type aliases, third-party maps, …) \
            have no TOML-faithful schema shape: either add `#[derive(clapfig::Schema)]` to the \
            type (structs with named fields and unit-only enums only), or mark the field \
            `#[clapfig(value)]` and take over the deserialize side yourself"
)]
pub trait Schema {
    /// The macro-emitted const schema tree. Const so it composes inside
    /// nested `static SchemaStatic = ...` initializers.
    const STATIC: &'static SchemaStatic;

    /// Convenience accessor; equivalent to `Self::STATIC`.
    fn schema_static() -> &'static SchemaStatic {
        Self::STATIC
    }

    /// Cached runtime view. The macro emits this method explicitly with a
    /// per-impl `OnceLock`; the helper [`cached_runtime_schema`] keeps the
    /// generated body small.
    fn schema() -> &'static RuntimeSchema;

    /// `Arc`-flavored access to the same cached runtime view. Used by the
    /// macro-driven builder ([`crate::TypedBuilder`]) to avoid
    /// cloning the schema tree per builder construction — the inner
    /// [`crate::Builder`] stores an `Arc<Schema>` and the cache hands out
    /// cheap reference-counted handles to it. Cost: one `Arc::clone` per
    /// call (atomic increment, no allocation).
    fn schema_arc() -> Arc<RuntimeSchema>;

    /// Flat list of every dotted path the schema knows about: leaf
    /// addressable keys plus every nested-section and array-of-objects
    /// node. Lets consumer code (extension registries, doc generators,
    /// `--list-keys` flags) replace hand-maintained "known paths"
    /// constants — the macro recomputes the list every time a field is
    /// added or removed.
    ///
    /// Ordering is depth-first, matching the order fields appear in the
    /// source struct (parents emit their own path before recursing into
    /// children). Array-of-objects nodes contribute the array name once
    /// (`"plugins"`); individual array entries are not addressable as
    /// distinct paths at this layer so no `plugins[N]` form is emitted.
    /// Unit-enum leaves contribute only their own path (the variant set
    /// is metadata on the leaf, not a separate sub-path).
    ///
    /// The default impl walks `Self::STATIC`. Override is rarely needed.
    fn field_paths() -> Vec<String> {
        let mut out = Vec::new();
        collect_field_paths(Self::STATIC, "", &mut out);
        out
    }
}

/// Derive-support marker: asserts a field type the macro claimed as a
/// datetime leaf really is [`clapfig::value::Datetime`](crate::value::Datetime).
///
/// The derive macro recognizes datetime fields *syntactically* — exactly
/// the spellings `Datetime`, `value::Datetime`, and
/// `clapfig::value::Datetime` are claimed as a `LeafTypeStatic::DateTime`
/// leaf (any other path, e.g. `my_mod::Datetime`, is not). Name resolution
/// isn't available to a proc macro, so a user's own use-imported
/// `struct Datetime` would otherwise be *silently* mis-typed as a datetime
/// leaf. The macro therefore emits an `IsClapfigDatetime` bound assertion
/// for every claimed match; a lookalike type fails to compile with this
/// trait's diagnostic instead.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not `clapfig::value::Datetime`",
    label = "field type claimed as a datetime leaf by #[derive(clapfig::Schema)]",
    note = "the derive matches datetime fields by type name, and only clapfig's own \
            `clapfig::value::Datetime` carries the schema's datetime semantics. If this is \
            your own type: spell it with a qualified path the derive won't claim (e.g. \
            `my_mod::Datetime`) and give it its own schema shape — derive \
            `clapfig::Schema` for it, or mark the field `#[clapfig(value)]`"
)]
pub trait IsClapfigDatetime {}
impl IsClapfigDatetime for crate::value::Datetime {}

/// Derive-support marker: asserts a field type the macro claimed as a
/// free-form value leaf really is [`clapfig::value::Value`](crate::value::Value).
/// Same rationale as [`IsClapfigDatetime`] — the macro matches exactly the
/// spellings `Value`, `value::Value`, and `clapfig::value::Value`, and
/// must not silently claim a user's own use-imported `Value` type.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not `clapfig::value::Value`",
    label = "field type claimed as a free-form value leaf by #[derive(clapfig::Schema)]",
    note = "the derive matches value fields by type name, and only clapfig's own \
            `clapfig::value::Value` carries the schema's free-form semantics. If this is \
            your own type: spell it with a qualified path the derive won't claim (e.g. \
            `my_mod::Value`) and give it its own schema shape — derive `clapfig::Schema` \
            for it, or mark the field `#[clapfig(value)]` and keep responsibility for the \
            deserialize side"
)]
pub trait IsClapfigValue {}
impl IsClapfigValue for crate::value::Value {}

/// Depth-first walk of a [`SchemaStatic`] tree that appends each node's
/// dotted path to `out`. Backs [`Schema::field_paths`]; exposed publicly
/// because consumers that already have a `&SchemaStatic` (rare — usually
/// they have the trait impl instead) can reuse the same traversal.
///
/// `prefix` is the dotted path of the *current* schema's parent (or `""`
/// at the root). Each child appends its own name to the prefix; for a
/// leaf the appended path is added to `out`, and for a nested or
/// array-of subtree the section path is added *before* recursing so
/// downstream consumers can use the list as a section/path inventory in
/// one read.
pub fn collect_field_paths(schema: &SchemaStatic, prefix: &str, out: &mut Vec<String>) {
    for field in schema.fields {
        let dotted = if prefix.is_empty() {
            field.name.to_string()
        } else {
            format!("{prefix}.{}", field.name)
        };
        match &field.field {
            FieldStatic::Leaf(_) => out.push(dotted),
            FieldStatic::Nested { schema: child, .. }
            | FieldStatic::ArrayOf { schema: child, .. }
            | FieldStatic::MapOf { schema: child, .. }
                if child.is_enum() =>
            {
                // Enum-kind nested (and array-of / map-of enum) schemas
                // flatten to a leaf at the runtime layer; surface them here
                // as a single leaf path, not a section + variant paths.
                out.push(dotted);
            }
            FieldStatic::Nested { schema: child, .. }
            | FieldStatic::ArrayOf { schema: child, .. }
            | FieldStatic::MapOf { schema: child, .. } => {
                out.push(dotted.clone());
                collect_field_paths(child, &dotted, out);
            }
        }
    }
}

/// Shared helper invoked by macro-generated [`Schema::schema`] bodies.
///
/// The cache holds `Arc<Schema>` (not `Schema`) so [`Schema::schema_arc`]
/// can hand out cheap reference-counted clones without re-running
/// `to_runtime()`. [`Schema::schema`] returns a `&'static Schema` by
/// dereferencing through the `Arc` — the deref is sound because the
/// `OnceLock` itself is `'static`.
pub fn cached_runtime_schema(
    cell: &'static OnceLock<Arc<RuntimeSchema>>,
    static_schema: &'static SchemaStatic,
) -> &'static RuntimeSchema {
    let arc: &'static Arc<RuntimeSchema> =
        cell.get_or_init(|| Arc::new(static_schema.to_runtime()));
    arc.as_ref()
}

/// `Arc`-returning counterpart to [`cached_runtime_schema`].
pub fn cached_runtime_schema_arc(
    cell: &'static OnceLock<Arc<RuntimeSchema>>,
    static_schema: &'static SchemaStatic,
) -> Arc<RuntimeSchema> {
    cell.get_or_init(|| Arc::new(static_schema.to_runtime()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    static EMPTY_DOC: &[&str] = &[];

    static MINIMAL_SCHEMA: SchemaStatic = SchemaStatic {
        name: "Minimal",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "port",
            field: FieldStatic::Leaf(LeafStatic {
                doc: EMPTY_DOC,
                ty: LeafTypeStatic::Integer {
                    min: None,
                    max: None,
                },
                default: Some(ValueStatic::Integer(8080)),
                optional: false,
                env: None,
            }),
        }],
        enum_variants: &[],
    };

    #[test]
    fn static_to_runtime_roundtrips_minimal_shape() {
        let s = MINIMAL_SCHEMA.to_runtime();
        assert_eq!(s.name, "Minimal");
        assert_eq!(s.fields.len(), 1);
        match &s.fields[0].field {
            RuntimeField::Leaf(leaf) => {
                assert!(matches!(
                    leaf.ty,
                    RuntimeLeafType::Integer {
                        min: None,
                        max: None
                    }
                ));
                assert_eq!(leaf.default, Some(Value::Integer(8080)));
                assert!(!leaf.optional);
            }
            other => panic!("expected Leaf, got {other:?}"),
        }
    }

    #[test]
    fn value_static_array_to_toml_recurses() {
        let v = ValueStatic::Array(&[
            ValueStatic::String("a"),
            ValueStatic::String("b"),
            ValueStatic::Integer(1),
        ]);
        let value = v.to_value();
        match value {
            Value::Array(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Value::String("a".into()));
                assert_eq!(items[2], Value::Integer(1));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn value_static_table_to_toml_preserves_keys() {
        let v = ValueStatic::Table(&[
            ("name", ValueStatic::String("x")),
            ("count", ValueStatic::Integer(3)),
        ]);
        match v.to_value() {
            Value::Map(t) => {
                assert_eq!(t.get("name").unwrap().as_str(), Some("x"));
                assert_eq!(t.get("count").unwrap().as_integer(), Some(3));
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    #[test]
    fn leaf_type_static_enum_to_runtime_carries_values() {
        let lt = LeafTypeStatic::Enum {
            values: &[
                ValueStatic::String("debug"),
                ValueStatic::String("info"),
                ValueStatic::String("warn"),
                ValueStatic::String("error"),
            ],
        };
        match lt.to_runtime() {
            RuntimeLeafType::Enum { values } => {
                assert_eq!(values.len(), 4);
                assert_eq!(values[0], Value::String("debug".into()));
            }
            other => panic!("expected Enum, got {other:?}"),
        }
    }

    static NESTED_INNER: SchemaStatic = SchemaStatic {
        name: "Inner",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "url",
            field: FieldStatic::Leaf(LeafStatic {
                doc: EMPTY_DOC,
                ty: LeafTypeStatic::String,
                default: None,
                optional: true,
                env: None,
            }),
        }],
        enum_variants: &[],
    };

    static NESTED_OUTER: SchemaStatic = SchemaStatic {
        name: "Outer",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "db",
            field: FieldStatic::Nested {
                schema: &NESTED_INNER,
                doc: EMPTY_DOC,
            },
        }],
        enum_variants: &[],
    };

    static ENUM_PDF_PAGE: SchemaStatic = SchemaStatic {
        name: "PdfPageSize",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[],
        enum_variants: &["a4", "letter"],
    };

    static ENUM_CONTAINER: SchemaStatic = SchemaStatic {
        name: "Doc",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "page_size",
            field: FieldStatic::Nested {
                schema: &ENUM_PDF_PAGE,
                doc: EMPTY_DOC,
            },
        }],
        enum_variants: &[],
    };

    #[test]
    fn enum_kind_static_flattens_to_runtime_leaf_enum() {
        let s = ENUM_CONTAINER.to_runtime();
        assert_eq!(s.fields.len(), 1);
        match &s.fields[0].field {
            RuntimeField::Leaf(leaf) => match &leaf.ty {
                RuntimeLeafType::Enum { values } => {
                    assert_eq!(values.len(), 2);
                    assert_eq!(values[0], Value::String("a4".into()));
                    assert_eq!(values[1], Value::String("letter".into()));
                }
                other => panic!("expected Enum, got {other:?}"),
            },
            other => panic!("expected Leaf (enum flattened), got {other:?}"),
        }
    }

    #[test]
    fn is_enum_distinguishes_struct_from_enum_schema() {
        assert!(!MINIMAL_SCHEMA.is_enum());
        assert!(ENUM_PDF_PAGE.is_enum());
    }

    static MAP_OF_ENUM_CONTAINER: SchemaStatic = SchemaStatic {
        name: "Levels",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "levels",
            field: FieldStatic::MapOf {
                schema: &ENUM_PDF_PAGE,
                doc: &["Per-target page size."],
            },
        }],
        enum_variants: &[],
    };

    #[test]
    fn map_of_enum_static_flattens_to_runtime_map_of_enum_leaf() {
        let s = MAP_OF_ENUM_CONTAINER.to_runtime();
        match &s.fields[0].field {
            RuntimeField::Leaf(leaf) => {
                // Field-site doc wins over the enum type's doc.
                assert_eq!(leaf.doc, vec!["Per-target page size.".to_string()]);
                match &leaf.ty {
                    RuntimeLeafType::Map(inner) => match inner.as_ref() {
                        RuntimeLeafType::Enum { values } => {
                            assert_eq!(values.len(), 2);
                            assert_eq!(values[0], Value::String("a4".into()));
                        }
                        other => panic!("expected Enum inside Map, got {other:?}"),
                    },
                    other => panic!("expected Map, got {other:?}"),
                }
            }
            other => panic!("expected Leaf (map-of-enum flattened), got {other:?}"),
        }
    }

    #[test]
    fn map_of_enum_contributes_a_single_leaf_path() {
        let mut out = Vec::new();
        collect_field_paths(&MAP_OF_ENUM_CONTAINER, "", &mut out);
        assert_eq!(out, vec!["levels".to_string()]);
    }

    static ARRAY_OF_ENUM_CONTAINER: SchemaStatic = SchemaStatic {
        name: "Sizes",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "sizes",
            field: FieldStatic::ArrayOf {
                schema: &ENUM_PDF_PAGE,
                doc: &["Accepted page sizes."],
            },
        }],
        enum_variants: &[],
    };

    #[test]
    fn array_of_enum_static_flattens_to_runtime_array_of_enum_leaf() {
        let s = ARRAY_OF_ENUM_CONTAINER.to_runtime();
        match &s.fields[0].field {
            RuntimeField::Leaf(leaf) => {
                // Field-site doc wins over the enum type's doc.
                assert_eq!(leaf.doc, vec!["Accepted page sizes.".to_string()]);
                match &leaf.ty {
                    RuntimeLeafType::Array(inner) => match inner.as_ref() {
                        RuntimeLeafType::Enum { values } => {
                            assert_eq!(values.len(), 2);
                            assert_eq!(values[0], Value::String("a4".into()));
                        }
                        other => panic!("expected Enum inside Array, got {other:?}"),
                    },
                    other => panic!("expected Array, got {other:?}"),
                }
            }
            other => panic!("expected Leaf (array-of-enum flattened), got {other:?}"),
        }
    }

    #[test]
    fn array_of_enum_contributes_a_single_leaf_path() {
        let mut out = Vec::new();
        collect_field_paths(&ARRAY_OF_ENUM_CONTAINER, "", &mut out);
        assert_eq!(out, vec!["sizes".to_string()]);
    }

    static ARRAY_OF_STRUCT_CONTAINER: SchemaStatic = SchemaStatic {
        name: "App",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "plugins",
            field: FieldStatic::ArrayOf {
                schema: &NESTED_INNER,
                doc: &["Installed plugins."],
            },
        }],
        enum_variants: &[],
    };

    #[test]
    fn array_of_struct_static_converts_to_runtime_array_of_with_field_doc() {
        let s = ARRAY_OF_STRUCT_CONTAINER.to_runtime();
        match &s.fields[0].field {
            RuntimeField::ArrayOf(item) => {
                assert_eq!(item.name, "Inner");
                // Field-site doc wins over the item type's doc.
                assert_eq!(item.doc, vec!["Installed plugins.".to_string()]);
            }
            other => panic!("expected ArrayOf, got {other:?}"),
        }
    }

    #[test]
    fn array_of_struct_contributes_section_and_child_paths() {
        let mut out = Vec::new();
        collect_field_paths(&ARRAY_OF_STRUCT_CONTAINER, "", &mut out);
        assert_eq!(out, vec!["plugins".to_string(), "plugins.url".to_string()]);
    }

    #[test]
    #[should_panic(expected = "is a struct, not a unit-only enum")]
    fn optional_array_of_struct_panics_with_drop_option_guidance() {
        // `Option<Vec<Struct>>` — the derive emits `Array(EnumRef)` because
        // it can't tell enum from struct; the struct kind is a deferred
        // authoring error at the first `schema()` call.
        static BAD_OPTIONAL_ARRAY: SchemaStatic = SchemaStatic {
            name: "App",
            doc: EMPTY_DOC,
            strict: None,
            fields: &[NamedFieldStatic {
                name: "plugins",
                field: FieldStatic::Leaf(LeafStatic {
                    doc: EMPTY_DOC,
                    ty: LeafTypeStatic::Array(&LeafTypeStatic::EnumRef {
                        schema: &NESTED_INNER,
                        field_name: "plugins",
                    }),
                    default: None,
                    optional: true,
                    env: None,
                }),
            }],
            enum_variants: &[],
        };
        let _ = BAD_OPTIONAL_ARRAY.to_runtime();
    }

    static FIELD_DOC_OUTER: SchemaStatic = SchemaStatic {
        name: "Outer",
        doc: EMPTY_DOC,
        strict: None,
        fields: &[NamedFieldStatic {
            name: "db",
            field: FieldStatic::Nested {
                schema: &NESTED_INNER,
                doc: &["Primary database."],
            },
        }],
        enum_variants: &[],
    };

    #[test]
    fn nested_field_site_doc_overrides_type_doc() {
        let s = FIELD_DOC_OUTER.to_runtime();
        match &s.fields[0].field {
            RuntimeField::Nested(inner) => {
                assert_eq!(inner.doc, vec!["Primary database.".to_string()]);
            }
            other => panic!("expected Nested, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "not a variant of enum `PdfPageSize`")]
    fn enum_ref_default_outside_variant_set_panics_with_named_field() {
        static BAD_DEFAULT: SchemaStatic = SchemaStatic {
            name: "Doc",
            doc: EMPTY_DOC,
            strict: None,
            fields: &[NamedFieldStatic {
                name: "page_size",
                field: FieldStatic::Leaf(LeafStatic {
                    doc: EMPTY_DOC,
                    ty: LeafTypeStatic::EnumRef {
                        schema: &ENUM_PDF_PAGE,
                        field_name: "page_size",
                    },
                    default: Some(ValueStatic::String("tabloid")),
                    optional: false,
                    env: None,
                }),
            }],
            enum_variants: &[],
        };
        let _ = BAD_DEFAULT.to_runtime();
    }

    #[test]
    fn nested_static_schemas_compose_via_static_reference() {
        let s = NESTED_OUTER.to_runtime();
        assert_eq!(s.fields.len(), 1);
        match &s.fields[0].field {
            RuntimeField::Nested(inner) => {
                assert_eq!(inner.name, "Inner");
                assert_eq!(inner.fields.len(), 1);
            }
            other => panic!("expected Nested, got {other:?}"),
        }
    }

    #[test]
    fn cached_runtime_schema_returns_same_pointer_across_calls() {
        static CELL: OnceLock<Arc<RuntimeSchema>> = OnceLock::new();
        let a = cached_runtime_schema(&CELL, &MINIMAL_SCHEMA);
        let b = cached_runtime_schema(&CELL, &MINIMAL_SCHEMA);
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn cached_runtime_schema_arc_shares_underlying_schema_with_ref_accessor() {
        static CELL: OnceLock<Arc<RuntimeSchema>> = OnceLock::new();
        let r = cached_runtime_schema(&CELL, &MINIMAL_SCHEMA);
        let a = cached_runtime_schema_arc(&CELL, &MINIMAL_SCHEMA);
        // Both accessors must yield the same in-memory schema — pointer
        // equality after deref through the Arc.
        assert!(std::ptr::eq(r, a.as_ref()));
    }
}
