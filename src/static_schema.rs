//! Static-form, const-friendly schema types — the emission target of the
//! `#[derive(clapfig::Schema)]` proc macro.
//!
//! The runtime-side [`Schema`](crate::runtime::Schema) holds owned
//! `String` / `Vec` / `toml::Value` data. That shape is convenient for a
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
use toml::Value as TomlValue;

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
/// flattening a `FieldStatic::Nested(...)`: a non-empty list becomes a
/// `Field::Leaf` with `LeafType::Enum`, while an empty list keeps the
/// nested-object shape.
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
#[derive(Debug)]
pub enum FieldStatic {
    Leaf(LeafStatic),
    Nested(&'static SchemaStatic),
    ArrayOf(&'static SchemaStatic),
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
    /// Signed 64-bit integer (TOML's only integer width).
    ///
    /// The derive macro maps every Rust integer type, including the
    /// unsigned ones (`u8`/`u16`/`u32`/`u64`/`usize`) and `isize`, to
    /// this variant. Values that exceed `i64::MAX` (e.g. a `u64`
    /// holding 2^63) **cannot be represented in TOML at all** — the
    /// failure mode is at serialize time, before the value ever
    /// reaches a deserializer, and there is no faithful intermediate.
    /// Field types like `u64` are accepted because they are convenient
    /// and round-trip correctly for the overwhelming majority of
    /// values; callers who need the full unsigned-64 range should
    /// store them as `String` and parse explicitly.
    ///
    /// `i128` and `u128` are rejected at derive time with a compile
    /// error rather than silently truncated.
    Integer,
    Float,
    Bool,
    DateTime,
    Array(&'static LeafTypeStatic),
    Map(&'static LeafTypeStatic),
    Enum {
        values: &'static [ValueStatic],
    },
    Value,
}

/// `const`-friendly mirror of `toml::Value` for default-value emission.
///
/// Datetimes are stored as their string form and parsed on conversion,
/// since `toml::value::Datetime` is not `const`-constructible.
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

impl FieldStatic {
    fn to_runtime(&self) -> RuntimeField {
        match self {
            FieldStatic::Leaf(leaf) => RuntimeField::Leaf(leaf.to_runtime()),
            // Flatten an enum-kind nested schema (a unit-only enum that
            // derived `Schema`) into a runtime leaf carrying the variant
            // list. The macro can't tell at parse time whether a field's
            // type is a struct or an enum — so it always emits
            // `FieldStatic::Nested(<T as Schema>::STATIC)`, and the kind
            // distinction happens here.
            FieldStatic::Nested(s) if s.is_enum() => RuntimeField::Leaf(RuntimeLeaf {
                doc: s.doc.iter().map(|d| (*d).to_string()).collect(),
                ty: RuntimeLeafType::Enum {
                    values: s
                        .enum_variants
                        .iter()
                        .map(|v| TomlValue::String((*v).to_string()))
                        .collect(),
                },
                default: None,
                optional: false,
                env: None,
            }),
            FieldStatic::Nested(s) => RuntimeField::Nested(s.to_runtime()),
            FieldStatic::ArrayOf(s) => RuntimeField::ArrayOf(s.to_runtime()),
        }
    }
}

impl LeafStatic {
    fn to_runtime(&self) -> RuntimeLeaf {
        RuntimeLeaf {
            doc: self.doc.iter().map(|s| (*s).to_string()).collect(),
            ty: self.ty.to_runtime(),
            default: self.default.as_ref().map(ValueStatic::to_toml),
            optional: self.optional,
            env: self.env.map(|s| s.to_string()),
        }
    }
}

impl LeafTypeStatic {
    pub fn to_runtime(&self) -> RuntimeLeafType {
        match self {
            LeafTypeStatic::String => RuntimeLeafType::String,
            LeafTypeStatic::Integer => RuntimeLeafType::Integer,
            LeafTypeStatic::Float => RuntimeLeafType::Float,
            LeafTypeStatic::Bool => RuntimeLeafType::Bool,
            LeafTypeStatic::DateTime => RuntimeLeafType::DateTime,
            LeafTypeStatic::Array(elem) => RuntimeLeafType::Array(Box::new(elem.to_runtime())),
            LeafTypeStatic::Map(v) => RuntimeLeafType::Map(Box::new(v.to_runtime())),
            LeafTypeStatic::Enum { values } => RuntimeLeafType::Enum {
                values: values.iter().map(ValueStatic::to_toml).collect(),
            },
            LeafTypeStatic::Value => RuntimeLeafType::Value,
        }
    }
}

impl ValueStatic {
    pub fn to_toml(&self) -> TomlValue {
        match self {
            ValueStatic::String(s) => TomlValue::String((*s).to_string()),
            ValueStatic::Integer(i) => TomlValue::Integer(*i),
            ValueStatic::Float(f) => TomlValue::Float(*f),
            ValueStatic::Bool(b) => TomlValue::Boolean(*b),
            ValueStatic::Datetime(s) => TomlValue::Datetime(
                s.parse()
                    .expect("clapfig: invalid datetime literal in static schema default"),
            ),
            ValueStatic::Array(items) => {
                TomlValue::Array(items.iter().map(ValueStatic::to_toml).collect())
            }
            ValueStatic::Table(entries) => {
                let mut t = toml::map::Map::new();
                for (k, v) in entries.iter() {
                    t.insert((*k).to_string(), v.to_toml());
                }
                TomlValue::Table(t)
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
    /// macro-driven builder ([`crate::SchemaConfigBuilder`]) to avoid
    /// cloning the schema tree per builder construction — the runtime
    /// spec stores an `Arc<Schema>` and the cache hands out cheap
    /// reference-counted handles to it. Cost: one `Arc::clone` per call
    /// (atomic increment, no allocation).
    fn schema_arc() -> Arc<RuntimeSchema>;
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
                ty: LeafTypeStatic::Integer,
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
                assert!(matches!(leaf.ty, RuntimeLeafType::Integer));
                assert_eq!(leaf.default, Some(TomlValue::Integer(8080)));
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
        let toml = v.to_toml();
        match toml {
            TomlValue::Array(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], TomlValue::String("a".into()));
                assert_eq!(items[2], TomlValue::Integer(1));
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
        match v.to_toml() {
            TomlValue::Table(t) => {
                assert_eq!(t.get("name").unwrap().as_str(), Some("x"));
                assert_eq!(t.get("count").unwrap().as_integer(), Some(3));
            }
            other => panic!("expected Table, got {other:?}"),
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
                assert_eq!(values[0], TomlValue::String("debug".into()));
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
            field: FieldStatic::Nested(&NESTED_INNER),
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
            field: FieldStatic::Nested(&ENUM_PDF_PAGE),
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
                    assert_eq!(values[0], TomlValue::String("a4".into()));
                    assert_eq!(values[1], TomlValue::String("letter".into()));
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
