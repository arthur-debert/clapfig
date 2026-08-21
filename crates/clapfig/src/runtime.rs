//! Runtime-defined schemas: owned [`Shape`] / [`Schema`] / [`LeafType`]
//! types and a fluent builder, for callers without a compile-time
//! `#[derive(clapfig::Schema)]` struct.
//!
//! [`Shape`] is the schema node (ADR-0010, `docs/spec/shape-algebra.md`):
//! leaf, named-field object, homogeneous map, homogeneous array, or
//! internally tagged union. [`Schema`] is the named-field **object**
//! constructor (`Schema::object`), not the node, and is not renamed to
//! `Object`. [`clapfig::Schema`](crate::Schema) is the derive trait.
//! An object's field value is a [`Shape`] — there is no second field-node
//! enum. Homogeneous maps and arrays of leaves or of objects are the same
//! [`Shape::Map`] / [`Shape::Array`] constructor with a different item.
//!
//! Walkers take [`Shape`]. [`Clapfig::builder`](crate::Clapfig::builder)
//! accepts `impl Into<Shape>` so object-root callers keep passing a
//! [`Schema`]. Root Map loads as a homogeneous map of the item shape.
//! Tagged object-root and nested tagged objects load via the two-phase
//! unknown-key walk.
//!
//! # Example
//!
//! ```ignore
//! use clapfig::runtime::{Field, Schema};
//!
//! let schema = Schema::object("AppConfig")
//!     .doc("Top-level application config.")
//!     .field("host", Field::string().doc("App host").default("localhost"))
//!     .field("port", Field::integer().default(8080i64))
//!     .field(
//!         "level",
//!         Field::enum_of(["debug", "info", "warn", "error"])
//!             .doc("Log verbosity")
//!             .default("info"),
//!     )
//!     .nested(
//!         "db",
//!         Schema::object("Db")
//!             .doc("Database settings")
//!             .field("url", Field::string().optional())
//!             .field("pool_size", Field::integer().default(5i64)),
//!     )
//!     .build();
//! ```

use crate::value::{Map, Value};

/// Named-field object — the [`Shape::Object`] constructor, not the schema
/// node (ADR-0010).
///
/// Constructed via [`Schema::object`] and the fluent builder, or directly
/// as a plain data struct. Object-root callers hand this to
/// [`Clapfig::builder`](crate::Clapfig::builder), which takes
/// `impl Into<Shape>` and walks the resulting [`Shape::Object`]. Each
/// field's value is a [`Shape`].
#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    pub doc: Vec<String>,
    /// Per-node strictness override. Phase 2 stores the value; Phase 3
    /// (cascading strictness) consumes it during unknown-key resolution.
    pub strict: Option<bool>,
    pub fields: Vec<NamedField>,
}

impl Schema {
    /// Start building a schema with the given object name (the derive
    /// macro emits the struct's name here).
    pub fn object(name: impl Into<String>) -> SchemaBuilder {
        SchemaBuilder {
            schema: Schema {
                name: name.into(),
                doc: Vec::new(),
                strict: None,
                fields: Vec::new(),
            },
        }
    }
}

/// Fluent builder for [`Schema`].
#[derive(Debug, Clone)]
pub struct SchemaBuilder {
    schema: Schema,
}

impl SchemaBuilder {
    /// Append a doc-comment line. Multiple calls accumulate; mirrors the
    /// effect of multi-line `///` comments on a static struct.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        self.schema.doc.push(line.into());
        self
    }

    /// Set the node-level `strict` override. Phase 3 (cascading strictness)
    /// consumes this during unknown-key resolution.
    pub fn strict(mut self, value: bool) -> Self {
        self.schema.strict = Some(value);
        self
    }

    /// Add a field whose value is any [`Shape`].
    ///
    /// Typical call sites pass a [`FieldBuilder`] (`Field::string()`,
    /// `Field::array_of_type(...)`, …). `name` is treated as a single
    /// TOML key and cannot contain `.` (the dotted-path separator), `[`,
    /// or `]` (array-index syntax), and cannot be empty. Violating this
    /// panics — the cost of constructing a schema with an ambiguous
    /// segment now is strictly less than the cost of debugging silent
    /// `KeyNotFound`s at every consumer (the resolve pipeline, persist,
    /// cascade lookup) down the line.
    pub fn field(mut self, name: impl Into<String>, field: impl Into<Shape>) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        self.schema.fields.push(NamedField {
            name,
            field: field.into(),
        });
        self
    }

    /// Add a nested object (TOML `[section]`). Same `name` constraints as
    /// [`field`](Self::field).
    pub fn nested(mut self, name: impl Into<String>, child: SchemaBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        self.schema.fields.push(NamedField {
            name,
            field: Shape::Object(child.build()),
        });
        self
    }

    /// Add an array of nested objects (TOML `[[name]]`). Same `name`
    /// constraints as [`field`](Self::field). A map or array of leaves
    /// uses [`Field::array_of_type`] instead — both are [`Shape::Array`]
    /// with a different item.
    pub fn array_of(mut self, name: impl Into<String>, item: SchemaBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        let item_schema = item.build();
        self.schema.fields.push(NamedField {
            name: name.clone(),
            field: Shape::Array(ArrayShape::of_object(name, item_schema)),
        });
        self
    }

    /// Add a string-keyed map of nested objects (TOML `[name.<key>]`).
    /// Same `name` constraints as [`field`](Self::field).
    ///
    /// Each entry's value must itself satisfy `item`'s schema — type
    /// checks, required-field enforcement, and nested unknown-key
    /// detection all recurse into entry tables. Keys are arbitrary
    /// user-supplied strings, so the cascade walks the item schema
    /// rather than the map level for strictness purposes. A map of
    /// leaves uses [`Field::map_of`] — both are [`Shape::Map`] with a
    /// different item.
    pub fn map_of(mut self, name: impl Into<String>, item: SchemaBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        let item_schema = item.build();
        self.schema.fields.push(NamedField {
            name: name.clone(),
            field: Shape::Map(MapShape::of_object(name, item_schema)),
        });
        self
    }

    /// Finalize the builder into a [`Schema`].
    pub fn build(self) -> Schema {
        self.schema
    }
}

/// A named field on a [`Schema`]. The field's value is a [`Shape`]
/// (ADR-0010) — not a second node type.
#[derive(Debug, Clone)]
pub struct NamedField {
    pub name: String,
    pub field: Shape,
}

/// Constructor namespace for field-position shapes (`Field::string()`,
/// `Field::array_of_type(...)`, …).
///
/// An object's field value is a [`Shape`]; there is no second field-node
/// enum (ADR-0010). Homogeneous arrays and maps of leaves are
/// [`Shape::Array`] / [`Shape::Map`] with a leaf item — the same
/// constructors [`SchemaBuilder::array_of`] / [`SchemaBuilder::map_of`]
/// use with an object item.
pub enum Field {}

impl Field {
    /// Start a leaf builder for a string value.
    pub fn string() -> FieldBuilder {
        FieldBuilder::new(LeafType::String)
    }

    /// Start a leaf builder for an integer value (unbounded — the full
    /// signed 64-bit range). Construct
    /// [`LeafType::Integer`] directly to declare bounds.
    pub fn integer() -> FieldBuilder {
        FieldBuilder::new(LeafType::Integer {
            min: None,
            max: None,
        })
    }

    /// Start a leaf builder for a range-bounded integer value. `None` on
    /// either end leaves that end open. Out-of-range values fail schema
    /// validation naming the key, and JSON Schema export carries the
    /// bounds as `minimum`/`maximum` — the runtime-schema counterpart of
    /// the width bounds the derive macro emits for sized integer fields.
    ///
    /// Panics if both ends are set and `min > max` — the same class of
    /// construction-time check as [`SchemaBuilder::field`] field names.
    pub fn integer_in(min: Option<i64>, max: Option<i64>) -> FieldBuilder {
        if let (Some(lo), Some(hi)) = (min, max) {
            assert!(
                lo <= hi,
                "clapfig: integer_in min ({lo}) must be <= max ({hi})"
            );
        }
        FieldBuilder::new(LeafType::Integer { min, max })
    }

    /// Start a leaf builder for a floating-point value.
    pub fn float() -> FieldBuilder {
        FieldBuilder::new(LeafType::Float)
    }

    /// Start a leaf builder for a boolean value.
    pub fn boolean() -> FieldBuilder {
        FieldBuilder::new(LeafType::Bool)
    }

    /// Start a leaf builder for a datetime value (TOML's four lexical
    /// forms).
    pub fn datetime() -> FieldBuilder {
        FieldBuilder::new(LeafType::DateTime)
    }

    /// Start a builder for a homogeneous array whose item is `item`.
    ///
    /// A `LeafType` or [`FieldBuilder`] converts to a leaf item; an
    /// object item is [`SchemaBuilder::array_of`]. Both are
    /// [`Shape::Array`].
    pub fn array_of_type(item: impl Into<Shape>) -> FieldBuilder {
        FieldBuilder::from_shape(Shape::Array(ArrayShape::of_item(item.into())))
    }

    /// Start a builder for a string-keyed map whose values are `value`.
    ///
    /// A `LeafType` or [`FieldBuilder`] converts to a leaf item; an
    /// object item is [`SchemaBuilder::map_of`]. Both are [`Shape::Map`].
    pub fn map_of(value: impl Into<Shape>) -> FieldBuilder {
        FieldBuilder::from_shape(Shape::Map(MapShape::of_item(value.into())))
    }

    /// Start a leaf builder constrained to one of `values`.
    ///
    /// Each `value` must be representable as a baseline primitive (string,
    /// integer, float, or bool). At load time, a merged value not in this
    /// set produces [`ClapfigError::InvalidValue`](crate::error::ClapfigError::InvalidValue).
    pub fn enum_of<V: Into<Value>, I: IntoIterator<Item = V>>(values: I) -> FieldBuilder {
        let values: Vec<Value> = values.into_iter().map(Into::into).collect();
        FieldBuilder::new(LeafType::Enum { values })
    }

    /// Start a leaf builder that accepts any config value.
    ///
    /// Escape hatch for keys whose value can take multiple incompatible
    /// shapes (e.g. a bare string *or* an array, like serde's
    /// `#[serde(untagged)]` enums). Clapfig will not type-check the value
    /// at this layer; the caller is responsible for any further validation
    /// or deserialization, typically inside a `post_validate` hook or
    /// after `load()` / `Resolver::resolve_at`.
    ///
    /// Strict mode is unaffected — `Value` is about *value shape* on a
    /// known key, not about whether unknown sibling keys are allowed.
    pub fn value() -> FieldBuilder {
        FieldBuilder::new(LeafType::Value)
    }
}

/// Owned leaf data for a runtime field.
#[derive(Debug, Clone)]
pub struct Leaf {
    pub doc: Vec<String>,
    pub ty: LeafType,
    pub default: Option<Value>,
    /// `true` if the field may be absent after merge. `false` (the default)
    /// makes it required — a `MissingRequired` error is produced if every
    /// layer omits the field and no default is set.
    pub optional: bool,
    /// Optional explicit env-var name override. Without this, the env layer
    /// derives names from the field path (`PREFIX__SECTION__FIELD`).
    pub env: Option<String>,
}

/// Leaf type discriminant — the value-level shape clapfig validates.
#[derive(Debug, Clone)]
pub enum LeafType {
    String,
    /// Signed 64-bit integer, optionally range-bounded. The derive macro
    /// emits the source Rust width's range here (`u8` → `0..=255`), so
    /// out-of-range values fail the schema check with the key path
    /// instead of a post-merge deserialize failure. `None` on either end
    /// leaves that end open ([`Field::integer`] leaves both open).
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float,
    Bool,
    /// Datetime in one of the baseline's four lexical forms (offset
    /// date-time, local date-time, local date, local time). String values
    /// matching one of the forms are coerced during finalization —
    /// schema-driven coercion, per ADR-0001.
    DateTime,
    /// Constrained value: must equal one of the listed values.
    Enum {
        values: Vec<Value>,
    },
    /// Accept any config value (scalar, array, map). Clapfig performs no
    /// shape check; the caller is responsible for further validation,
    /// typically via `serde` in a `post_validate` hook. Used for keys
    /// whose value can take multiple incompatible shapes on the same
    /// field (e.g. a bare string *or* an array of `[string, table]`).
    Value,
}

impl LeafType {
    /// Human-readable name for use in error messages.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            LeafType::String => "string",
            LeafType::Integer { .. } => "integer",
            LeafType::Float => "float",
            LeafType::Bool => "bool",
            LeafType::DateTime => "datetime",
            LeafType::Enum { .. } => "enum",
            LeafType::Value => "value",
        }
    }

    /// Check whether a [`Value`] is shape-compatible with this leaf type.
    ///
    /// Homogeneous arrays and maps are [`Shape::Array`] / [`Shape::Map`]
    /// and are checked by the Shape walker, not here. `Enum` checks
    /// literal equality against the allowed-value set. `Integer` also
    /// enforces its declared bounds, and `Float` accepts integer values
    /// (serde accepts them for `f64` fields; the finalize pass coerces the
    /// stored value to a float). Returns `Ok(())` on match; on mismatch,
    /// returns a human-readable reason suitable for
    /// `ClapfigError::InvalidValue::reason`.
    pub(crate) fn check(&self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (LeafType::String, Value::String(_)) => Ok(()),
            (LeafType::Integer { min, max }, Value::Integer(i)) => {
                if min.is_some_and(|lo| *i < lo) || max.is_some_and(|hi| *i > hi) {
                    Err(format!(
                        "value {i} is out of range (allowed: {})",
                        format_bounds(*min, *max)
                    ))
                } else {
                    Ok(())
                }
            }
            (LeafType::Float, Value::Float(_)) => Ok(()),
            // Integer-for-float: serde accepts `timeout = 5` for an `f64`
            // field, so the schema check does too. The finalize pass
            // rewrites the value to `Value::Float` (see
            // `schema_walk::coerce_value`).
            (LeafType::Float, Value::Integer(_)) => Ok(()),
            (LeafType::Bool, Value::Boolean(_)) => Ok(()),
            (LeafType::DateTime, Value::Datetime(_)) => Ok(()),
            (LeafType::Enum { values }, v) => {
                if values.iter().any(|allowed| allowed == v) {
                    Ok(())
                } else {
                    let listed = values
                        .iter()
                        .map(format_value)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    Err(format!(
                        "value {} is not in allowed set: {listed}",
                        format_value(v)
                    ))
                }
            }
            (LeafType::Value, _) => Ok(()),
            (expected, got) => Err(format!(
                "expected {}, got {}",
                expected.name(),
                value_type_name(got)
            )),
        }
    }
}

/// Fluent builder for a field-position [`Shape`] (typically a leaf, or a
/// homogeneous map/array of leaves).
#[derive(Debug, Clone)]
pub struct FieldBuilder {
    shape: Shape,
}

impl FieldBuilder {
    fn new(ty: LeafType) -> Self {
        Self {
            shape: Shape::Leaf(Leaf {
                doc: Vec::new(),
                ty,
                default: None,
                optional: false,
                env: None,
            }),
        }
    }

    fn from_shape(shape: Shape) -> Self {
        Self { shape }
    }

    /// Append a doc-comment line.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        match &mut self.shape {
            Shape::Leaf(leaf) => leaf.doc.push(line.into()),
            Shape::Map(map) => map.doc.push(line.into()),
            Shape::Array(array) => array.doc.push(line.into()),
            other => panic!(
                "clapfig: .doc() is only valid on Leaf, Map, and Array, got {}",
                other.constructor_name()
            ),
        }
        self
    }

    /// Set the default value injected when no layer supplies one.
    pub fn default<V: Into<Value>>(mut self, value: V) -> Self {
        let value = value.into();
        match &mut self.shape {
            Shape::Leaf(leaf) => leaf.default = Some(value),
            Shape::Map(map) => map.default = Some(value),
            Shape::Array(array) => array.default = Some(value),
            other => panic!(
                "clapfig: .default() is only valid on Leaf, Map, and Array, got {}",
                other.constructor_name()
            ),
        }
        self
    }

    /// Mark this field optional — absence after merge is accepted.
    pub fn optional(mut self) -> Self {
        match &mut self.shape {
            Shape::Leaf(leaf) => leaf.optional = true,
            Shape::Map(map) => map.optional = true,
            Shape::Array(array) => array.optional = true,
            other => panic!(
                "clapfig: .optional() is only valid on Leaf, Map, and Array, got {}",
                other.constructor_name()
            ),
        }
        self
    }

    /// Override the env-var name for this field. Without this, the env layer
    /// derives a name from the field path.
    pub fn env(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        match &mut self.shape {
            Shape::Leaf(leaf) => leaf.env = Some(name),
            Shape::Map(map) => map.env = Some(name),
            Shape::Array(array) => array.env = Some(name),
            other => panic!(
                "clapfig: .env() is only valid on Leaf, Map, and Array, got {}",
                other.constructor_name()
            ),
        }
        self
    }

    pub(crate) fn build(self) -> Shape {
        self.shape
    }
}

/// A schema node: leaf, named-field object, homogeneous map, homogeneous
/// array, or internally tagged union.
///
/// This is the node walkers take. [`Schema`] remains the named-field
/// object — the [`Shape::Object`] constructor — not this enum and not
/// renamed to `Object` (ADR-0010). An object's field value is a
/// [`Shape`]; there is no second field-node enum. Homogeneous maps and
/// arrays of leaves or of objects are the same [`Map`](Shape::Map) /
/// [`Array`](Shape::Array) constructor with a different item.
///
/// Legal document roots are [`Object`](Shape::Object), [`Map`](Shape::Map),
/// and [`Tagged`](Shape::Tagged). [`Leaf`](Shape::Leaf) and
/// [`Array`](Shape::Array) are valid nested shapes; using them as a
/// document root is rejected at construction.
///
/// ```
/// use clapfig::runtime::{Field, Schema, Shape};
///
/// let item = Schema::object("Block")
///     .field("mount", Field::string())
///     .build();
/// let root = Shape::from(Shape::map("blocks", item).build());
/// assert!(root.is_legal_document_root());
/// ```
#[derive(Debug, Clone)]
pub enum Shape {
    /// A scalar / closed-enum / `Value`-escape-hatch leaf. Not a legal
    /// document root.
    Leaf(Leaf),
    /// Today's named-field object ([`Schema::object`]). A legal document
    /// root.
    Object(Schema),
    /// Homogeneous unordered string-keyed map. Item is any shape. A legal
    /// document root.
    Map(MapShape),
    /// Homogeneous array. Item is any shape. Not a legal document root.
    Array(ArrayShape),
    /// Internally tagged union of objects, selected by a discriminator
    /// field. A legal document root. JSON Schema is `oneOf` with a
    /// per-branch tag `const`; `config gen` emits one commented example
    /// per variant.
    Tagged(TaggedShape),
}

/// Document-root constructor the resolve pipeline walks.
///
/// Object, Map, and Tagged document roots load.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DocumentRoot<'a> {
    Object(&'a Schema),
    Map(&'a MapShape),
    Tagged(&'a TaggedShape),
}

impl Shape {
    /// A leaf node from a [`LeafType`]. Not a legal document root.
    pub fn leaf(ty: LeafType) -> Self {
        Self::Leaf(Leaf {
            doc: Vec::new(),
            ty,
            default: None,
            optional: false,
            env: None,
        })
    }

    /// Wrap a named-field object. A legal document root.
    pub fn object(schema: Schema) -> Self {
        Self::Object(schema)
    }

    /// Start a homogeneous map. A legal document root.
    ///
    /// `name` is the node name (the derive will emit the type name here).
    /// `item` is any shape — a map of objects and a map of leaves are the
    /// same constructor.
    pub fn map(name: impl Into<String>, item: impl Into<Shape>) -> MapShapeBuilder {
        MapShapeBuilder {
            map: MapShape {
                name: name.into(),
                doc: Vec::new(),
                strict: None,
                item: Box::new(item.into()),
                default: None,
                optional: false,
                env: None,
            },
        }
    }

    /// Start a homogeneous array. Not a legal document root.
    pub fn array(name: impl Into<String>, item: impl Into<Shape>) -> ArrayShapeBuilder {
        ArrayShapeBuilder {
            array: ArrayShape {
                name: name.into(),
                doc: Vec::new(),
                strict: None,
                item: Box::new(item.into()),
                default: None,
                optional: false,
                env: None,
            },
        }
    }

    /// Start an internally tagged union. A legal document root.
    ///
    /// `name` is the node name (the enum type name). `tag` is the
    /// discriminator field (`serde(tag = ...)`), reserved on every variant
    /// object and not itself a field of the variant. Panics if `tag` is
    /// empty or contains `.`, `[`, or `]` — the same path-segment rules
    /// as a field name, because the tag is a field key. Call
    /// [`TaggedShapeBuilder::variant`] at least once before
    /// [`build`](TaggedShapeBuilder::build).
    pub fn tagged(name: impl Into<String>, tag: impl Into<String>) -> TaggedShapeBuilder {
        let tag = tag.into();
        validate_path_segment("tagged discriminator field name", &tag);
        TaggedShapeBuilder {
            name: name.into(),
            doc: Vec::new(),
            strict: None,
            tag,
            variants: Vec::new(),
        }
    }

    /// Constructor name for error messages (`"Leaf"`, `"Object"`, …).
    pub fn constructor_name(&self) -> &'static str {
        match self {
            Shape::Leaf(_) => "Leaf",
            Shape::Object(_) => "Object",
            Shape::Map(_) => "Map",
            Shape::Array(_) => "Array",
            Shape::Tagged(_) => "Tagged",
        }
    }

    /// Object, Map, and Tagged are legal document roots; Leaf and Array
    /// are not.
    pub fn is_legal_document_root(&self) -> bool {
        matches!(self, Shape::Object(_) | Shape::Map(_) | Shape::Tagged(_))
    }

    /// Panic unless this shape is a legal document root.
    ///
    /// Leaf and Array are valid nested shapes; they are not documents.
    pub fn require_document_root(&self) {
        match self {
            Shape::Object(_) | Shape::Map(_) | Shape::Tagged(_) => {}
            Shape::Leaf(_) => panic!(
                "clapfig: a Leaf is not a legal document root (legal roots: Object, Map, Tagged)"
            ),
            Shape::Array(_) => panic!(
                "clapfig: an Array is not a legal document root (legal roots: Object, Map, Tagged)"
            ),
        }
    }

    /// Field-site doc lines (leaf / map / array header, or the object's
    /// own doc).
    pub(crate) fn field_doc(&self) -> &[String] {
        match self {
            Shape::Leaf(leaf) => &leaf.doc,
            Shape::Object(schema) => &schema.doc,
            Shape::Map(map) => &map.doc,
            Shape::Array(array) => &array.doc,
            Shape::Tagged(tagged) => &tagged.doc,
        }
    }

    /// True when this field renders and address as a value (scalar, enum,
    /// `Value`, or a homogeneous array/map of those) rather than a nested
    /// object, array-of-tables, or map-of-objects.
    pub(crate) fn is_value_field(&self) -> bool {
        match self {
            Shape::Leaf(_) => true,
            Shape::Array(array) => array.item.is_value_field(),
            Shape::Map(map) => map.item.is_value_field(),
            Shape::Object(_) | Shape::Tagged(_) => false,
        }
    }

    /// Follow Array/Map items until a named node (Object, Leaf, or Tagged).
    ///
    /// Anonymous container nodes do not contribute dotted path segments, so
    /// lookups such as `strict_at` and `doc_for` walk through them without
    /// consuming another key.
    pub(crate) fn peel_containers(&self) -> &Shape {
        let mut current = self;
        loop {
            match current {
                Shape::Array(array) => current = array.item.as_ref(),
                Shape::Map(map) => current = map.item.as_ref(),
                Shape::Leaf(_) | Shape::Object(_) | Shape::Tagged(_) => return current,
            }
        }
    }

    /// Type-check `value` against this shape. Object / tagged payloads
    /// are checked field-by-field by the walker; this only asserts the
    /// container kind. Homogeneous arrays and maps recurse into items.
    pub(crate) fn check_value(&self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (Shape::Leaf(leaf), v) => leaf.ty.check(v),
            (Shape::Object(_), Value::Map(_)) => Ok(()),
            (Shape::Object(_), other) => {
                Err(format!("expected map, got {}", value_type_name(other)))
            }
            (Shape::Array(array), Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    array
                        .item
                        .check_value(item)
                        .map_err(|e| format!("array[{i}]: {e}"))?;
                }
                Ok(())
            }
            (Shape::Array(_), other) => {
                Err(format!("expected array, got {}", value_type_name(other)))
            }
            (Shape::Map(map), Value::Map(table)) => {
                for (k, v) in table {
                    map.item
                        .check_value(v)
                        .map_err(|e| format!("map[{k}]: {e}"))?;
                }
                Ok(())
            }
            (Shape::Map(_), other) => Err(format!("expected map, got {}", value_type_name(other))),
            (Shape::Tagged(_), Value::Map(_)) => Ok(()),
            (Shape::Tagged(_), other) => {
                Err(format!("expected map, got {}", value_type_name(other)))
            }
        }
    }

    /// Whether two shapes are the same persist target.
    ///
    /// Recurses through [`Array`](Shape::Array) / [`Map`](Shape::Map)
    /// items and compares [`Leaf`](Shape::Leaf) types (integer bounds and
    /// enum members included). Two **shallow** arms are load-bearing and
    /// are not accidental fallthrough:
    ///
    /// - [`Object`](Shape::Object) / [`Object`](Shape::Object) agrees
    ///   regardless of fields. Persist addresses a value, not a nested
    ///   object's contents; once both sides are objects the target is the
    ///   same kind of node.
    /// - [`Tagged`](Shape::Tagged) / [`Tagged`](Shape::Tagged) compares
    ///   the tag name only. Variant sets are not compared: a nested union
    ///   is the same persist target when it is selected by the same
    ///   discriminator field.
    ///
    /// Cross-constructor pairs never agree. This is not general shape
    /// equality — Object fields and Tagged variants stay uncompared on
    /// purpose. Persist is the consumer; do not reuse this as a schema
    /// diff.
    pub(crate) fn structurally_agrees_with(&self, other: &Shape) -> bool {
        match (self, other) {
            (Shape::Leaf(la), Shape::Leaf(lb)) => leaf_types_agree(&la.ty, &lb.ty),
            (Shape::Array(aa), Shape::Array(ab)) => aa.item.structurally_agrees_with(&ab.item),
            (Shape::Map(ma), Shape::Map(mb)) => ma.item.structurally_agrees_with(&mb.item),
            (Shape::Object(_), Shape::Object(_)) => true,
            (Shape::Tagged(ta), Shape::Tagged(tb)) => ta.tag == tb.tag,
            _ => false,
        }
    }
}

fn leaf_types_agree(a: &LeafType, b: &LeafType) -> bool {
    match (a, b) {
        (LeafType::String, LeafType::String)
        | (LeafType::Float, LeafType::Float)
        | (LeafType::Bool, LeafType::Bool)
        | (LeafType::DateTime, LeafType::DateTime)
        | (LeafType::Value, LeafType::Value) => true,
        (
            LeafType::Integer {
                min: a_min,
                max: a_max,
            },
            LeafType::Integer {
                min: b_min,
                max: b_max,
            },
        ) => a_min == b_min && a_max == b_max,
        (LeafType::Enum { values: a }, LeafType::Enum { values: b }) => a == b,
        _ => false,
    }
}

impl From<Schema> for Shape {
    fn from(schema: Schema) -> Self {
        Shape::Object(schema)
    }
}

impl From<SchemaBuilder> for Shape {
    fn from(builder: SchemaBuilder) -> Self {
        Shape::Object(builder.build())
    }
}

impl From<Leaf> for Shape {
    fn from(leaf: Leaf) -> Self {
        Shape::Leaf(leaf)
    }
}

impl From<FieldBuilder> for Shape {
    fn from(builder: FieldBuilder) -> Self {
        builder.build()
    }
}

impl From<LeafType> for Shape {
    fn from(ty: LeafType) -> Self {
        Shape::leaf(ty)
    }
}

impl From<MapShape> for Shape {
    fn from(map: MapShape) -> Self {
        Shape::Map(map)
    }
}

impl From<ArrayShape> for Shape {
    fn from(array: ArrayShape) -> Self {
        Shape::Array(array)
    }
}

impl From<TaggedShape> for Shape {
    fn from(tagged: TaggedShape) -> Self {
        Shape::Tagged(tagged)
    }
}

impl From<MapShapeBuilder> for Shape {
    fn from(builder: MapShapeBuilder) -> Self {
        Shape::Map(builder.build())
    }
}

impl From<ArrayShapeBuilder> for Shape {
    fn from(builder: ArrayShapeBuilder) -> Self {
        Shape::Array(builder.build())
    }
}

impl From<TaggedShapeBuilder> for Shape {
    fn from(builder: TaggedShapeBuilder) -> Self {
        Shape::Tagged(builder.build())
    }
}

/// Homogeneous unordered map: string keys, item is any [`Shape`].
///
/// Node header (`name` / `doc` / `strict`) parallels [`Schema`]. A legal
/// document root. When this map is a named field, `optional` / `default`
/// / `env` are the field-site attrs (a map of leaves used to carry them
/// on the collapsed `LeafType::Map` leaf).
#[derive(Debug, Clone)]
pub struct MapShape {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,
    pub item: Box<Shape>,
    pub default: Option<Value>,
    pub optional: bool,
    pub env: Option<String>,
}

impl MapShape {
    fn of_item(item: Shape) -> Self {
        Self {
            name: String::new(),
            doc: Vec::new(),
            strict: None,
            item: Box::new(item),
            default: None,
            optional: false,
            env: None,
        }
    }

    fn of_object(name: String, item: Schema) -> Self {
        Self {
            name,
            doc: item.doc.clone(),
            strict: None,
            item: Box::new(Shape::Object(item)),
            default: None,
            optional: false,
            env: None,
        }
    }
}

/// Fluent builder for [`MapShape`].
#[derive(Debug, Clone)]
pub struct MapShapeBuilder {
    map: MapShape,
}

impl MapShapeBuilder {
    /// Append a doc-comment line.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        self.map.doc.push(line.into());
        self
    }

    /// Set the node-level `strict` override.
    pub fn strict(mut self, value: bool) -> Self {
        self.map.strict = Some(value);
        self
    }

    /// Finalize into a [`MapShape`].
    pub fn build(self) -> MapShape {
        self.map
    }
}

/// Homogeneous array: item is any [`Shape`].
///
/// Not a legal document root. Node header parallels [`Schema`]. When this
/// array is a named field, `optional` / `default` / `env` are the
/// field-site attrs (a array of leaves used to carry them on the
/// collapsed `LeafType::Array` leaf).
#[derive(Debug, Clone)]
pub struct ArrayShape {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,
    pub item: Box<Shape>,
    pub default: Option<Value>,
    pub optional: bool,
    pub env: Option<String>,
}

impl ArrayShape {
    fn of_item(item: Shape) -> Self {
        Self {
            name: String::new(),
            doc: Vec::new(),
            strict: None,
            item: Box::new(item),
            default: None,
            optional: false,
            env: None,
        }
    }

    fn of_object(name: String, item: Schema) -> Self {
        Self {
            name,
            doc: item.doc.clone(),
            strict: None,
            item: Box::new(Shape::Object(item)),
            default: None,
            optional: false,
            env: None,
        }
    }
}

/// Fluent builder for [`ArrayShape`].
#[derive(Debug, Clone)]
pub struct ArrayShapeBuilder {
    array: ArrayShape,
}

impl ArrayShapeBuilder {
    /// Append a doc-comment line.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        self.array.doc.push(line.into());
        self
    }

    /// Set the node-level `strict` override.
    pub fn strict(mut self, value: bool) -> Self {
        self.array.strict = Some(value);
        self
    }

    /// Finalize into an [`ArrayShape`].
    pub fn build(self) -> ArrayShape {
        self.array
    }
}

/// Internally tagged union of objects.
///
/// The tag is declared here (serde `tag = "..."`) and is **not** a field
/// of any variant object. Each variant is a [`Schema`] (an object). A
/// unit variant is the empty object. Variants that are Map, Array, Leaf,
/// or Tagged are rejected at construction.
///
/// A legal document root. The two-phase tagged walk (pre-merge union of
/// variant fields, post-merge branch selection) is SHP01-WS04.
#[derive(Debug, Clone)]
pub struct TaggedShape {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,
    /// Discriminator field name. Required, closed, never an unknown key.
    pub tag: String,
    /// Closed discriminator set. At least one variant; names unique and
    /// non-empty.
    pub variants: Vec<TaggedVariant>,
}

/// One variant of a [`TaggedShape`]: post-rename discriminator → object.
#[derive(Debug, Clone)]
pub struct TaggedVariant {
    pub discriminator: String,
    pub schema: Schema,
}

/// How a single key is declared across a tagged union's variants.
///
/// Classification is shared; combination is not. The four consumers keep
/// distinct policies because they answer different questions — they must
/// not be unified:
///
/// - **schema_walk** (`collect_unknown_against_shapes_union`): a key is
///   known at phase 1 if **any** variant declares it (`Every` or
///   `Partial`). True unknowns are `Absent`. The tag is never unknown.
/// - **meta** (`agreed_doc`): the shared doc vector when every
///   participating variant has the same vector, including empty
///   vectors. `Partial` and `Every` both look up; they differ only in
///   how many docs participate. Disagreement yields `Some(vec![])`
///   (the key exists, no unique doc); `None` means the key is absent.
/// - **strict** (`union_path_kind`): path kind is the union across
///   variants that declare the path — section in any variant wins; a
///   leaf only when every declaration is a leaf.
/// - **persist** (`unambiguous_value_shapes`): a settable target only
///   when **every** variant declares the key (`Every`) and those
///   declarations [agree structurally](Shape::structurally_agrees_with).
///   `Partial` is unaddressable until a discriminator selects a branch.
///
/// "Unknown only if every variant says unknown" is not "settable only if
/// every variant agrees."
#[derive(Debug)]
pub(crate) enum KeyAcrossVariants<'a> {
    /// The discriminator field itself. Never an unknown key; not a
    /// variant field.
    Tag,
    /// No variant declares this name.
    Absent,
    /// Every variant declares this name. Shapes are in variant order.
    Every(Vec<&'a Shape>),
    /// Some but not all variants declare this name (branch-exclusive
    /// candidate). Shapes are in variant order among those that declare
    /// it.
    Partial(Vec<&'a Shape>),
}

impl TaggedShape {
    /// Variant whose discriminator matches `name`.
    pub(crate) fn variant(&self, name: &str) -> Option<&TaggedVariant> {
        self.variants.iter().find(|v| v.discriminator == name)
    }

    /// Selected variant after merge: the tag value is a string matching a
    /// closed discriminator. Missing or non-matching tags yield `None`
    /// (finalize reports `MissingRequired` / `InvalidValue`).
    pub(crate) fn selected<'a>(&'a self, table: &Map) -> Option<&'a TaggedVariant> {
        match table.get(&self.tag) {
            Some(Value::String(name)) => self.variant(name),
            _ => None,
        }
    }

    /// Closed discriminator set as a [`LeafType::Enum`] so unknown /
    /// mistyped tags reuse the leaf error wording (origin + allowed set).
    pub(crate) fn discriminator_leaf_type(&self) -> LeafType {
        LeafType::Enum {
            values: self
                .variants
                .iter()
                .map(|v| Value::String(v.discriminator.clone()))
                .collect(),
        }
    }

    /// Classify `key` once across this union's variants.
    ///
    /// Callers state a combination policy on the result; they do not walk
    /// [`Self::variants`] looking up a field name. See
    /// [`KeyAcrossVariants`] for the four policies and why they differ.
    pub(crate) fn resolve_key(&self, key: &str) -> KeyAcrossVariants<'_> {
        self.resolve_key_with(key, |a, b| a == b)
    }

    /// Same classification as [`Self::resolve_key`], comparing names with
    /// `eq`. Metadata lookups pass kebab/snake-equivalent matching; other
    /// consumers use exact equality via [`Self::resolve_key`].
    pub(crate) fn resolve_key_with(
        &self,
        key: &str,
        eq: impl Fn(&str, &str) -> bool,
    ) -> KeyAcrossVariants<'_> {
        if eq(&self.tag, key) {
            return KeyAcrossVariants::Tag;
        }
        let n = self.variants.len();
        let mut shapes = Vec::new();
        for variant in &self.variants {
            if let Some(field) = variant.schema.fields.iter().find(|f| eq(&f.name, key)) {
                shapes.push(&field.field);
            }
        }
        if shapes.is_empty() {
            KeyAcrossVariants::Absent
        } else if shapes.len() == n {
            KeyAcrossVariants::Every(shapes)
        } else {
            KeyAcrossVariants::Partial(shapes)
        }
    }
}

/// Fluent builder for [`TaggedShape`].
#[derive(Debug, Clone)]
pub struct TaggedShapeBuilder {
    name: String,
    doc: Vec<String>,
    strict: Option<bool>,
    tag: String,
    variants: Vec<TaggedVariant>,
}

impl TaggedShapeBuilder {
    /// Append a doc-comment line.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        self.doc.push(line.into());
        self
    }

    /// Set the node-level `strict` override.
    pub fn strict(mut self, value: bool) -> Self {
        self.strict = Some(value);
        self
    }

    /// Add a variant. `shape` must be [`Shape::Object`]; other constructors
    /// panic. `discriminator` is the post-rename name: non-empty and unique
    /// within this union. Panics if the variant object already has a
    /// (post-rename) field named the union tag — the tag is reserved, not
    /// a field of the variant.
    pub fn variant(mut self, discriminator: impl Into<String>, shape: impl Into<Shape>) -> Self {
        let discriminator = discriminator.into();
        assert!(
            !discriminator.is_empty(),
            "clapfig: tagged discriminator must not be empty"
        );
        assert!(
            !self
                .variants
                .iter()
                .any(|v| v.discriminator == discriminator),
            "clapfig: duplicate tagged discriminator {discriminator:?} on {:?}",
            self.name
        );
        let shape = shape.into();
        let schema = match shape {
            Shape::Object(schema) => schema,
            other => panic!(
                "clapfig: tagged variant {discriminator:?} must be an object, got {}",
                other.constructor_name()
            ),
        };
        reject_variant_tag_clash(
            &self.tag,
            &discriminator,
            schema.fields.iter().map(|f| f.name.as_str()),
        );
        self.variants.push(TaggedVariant {
            discriminator,
            schema,
        });
        self
    }

    /// Finalize into a [`TaggedShape`].
    ///
    /// Panics if no variant was added. Empty tag names are rejected by
    /// [`Shape::tagged`]; empty / duplicate discriminators and non-object
    /// variants are rejected by [`variant`](Self::variant).
    pub fn build(self) -> TaggedShape {
        assert!(
            !self.variants.is_empty(),
            "clapfig: tagged union {:?} must have at least one variant",
            self.name
        );
        TaggedShape {
            name: self.name,
            doc: self.doc,
            strict: self.strict,
            tag: self.tag,
            variants: self.variants,
        }
    }
}

/// Reject a single path-segment name (a field or a tagged-union tag).
///
/// `.` / `[` / `]` / empty break dotted-path lookup and array-index
/// syntax the same way for a discriminator field as for any other key.
pub(crate) fn validate_path_segment(label: &str, name: &str) {
    assert!(!name.is_empty(), "clapfig: {label} must not be empty");
    assert!(
        !name.contains('.'),
        "clapfig: {label} {name:?} contains '.', which conflicts with the dotted-path separator"
    );
    assert!(
        !name.contains('['),
        "clapfig: {label} {name:?} contains '[', which conflicts with array-index syntax"
    );
    assert!(
        !name.contains(']'),
        "clapfig: {label} {name:?} contains ']', which conflicts with array-index syntax"
    );
}

/// The tag is reserved on every variant object and is not a field of the
/// variant. A post-rename field named the same as the tag is an authoring
/// error (schema and serde would fight).
pub(crate) fn reject_variant_tag_clash<'a>(
    tag: &str,
    discriminator: &str,
    field_names: impl IntoIterator<Item = &'a str>,
) {
    for name in field_names {
        assert!(
            name != tag,
            "clapfig: tagged variant {discriminator:?} must not declare a field named {tag:?} (the union tag)"
        );
    }
}

/// Reject field names that would confuse every downstream consumer
/// (resolve, persist, cascade lookup) the moment they're constructed.
///
/// Path-segment rules are [`validate_path_segment`]. Duplicate names
/// within one schema make `find_field` order-dependent and `valid_keys`
/// collide.
fn validate_field_name(schema: &Schema, name: &str) {
    validate_path_segment("field name", name);
    assert!(
        !schema.fields.iter().any(|f| f.name == name),
        "clapfig: duplicate field name {name:?} on schema {:?}",
        schema.name
    );
}

/// Human-readable spelling of an integer leaf's declared bounds for error
/// messages: `0..=255`, `>= 0`, or `<= 100`. Callers never pass
/// `(None, None)` (an unbounded integer has no range to violate).
fn format_bounds(min: Option<i64>, max: Option<i64>) -> String {
    match (min, max) {
        (Some(lo), Some(hi)) => format!("{lo}..={hi}"),
        (Some(lo), None) => format!(">= {lo}"),
        (None, Some(hi)) => format!("<= {hi}"),
        (None, None) => unreachable!("unbounded integers cannot be out of range"),
    }
}

/// Pretty-print a [`Value`] for error messages.
fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => crate::value::lexical_string(d),
        Value::Array(_) => "<array>".into(),
        Value::Map(_) => "<map>".into(),
    }
}

/// Type name of a [`Value`] for error messages, in the same vocabulary as
/// [`LeafType::name`] (so "expected map, got string" reads consistently).
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_builds_a_simple_schema() {
        let s = Schema::object("App")
            .doc("Top-level config")
            .field("host", Field::string().default("localhost"))
            .field("port", Field::integer().default(8080i64))
            .build();
        assert_eq!(s.name, "App");
        assert_eq!(s.doc, vec!["Top-level config".to_string()]);
        assert_eq!(s.fields.len(), 2);
        assert!(matches!(s.fields[0].field, Shape::Leaf(_)));
    }

    #[test]
    fn builder_handles_nested_schemas() {
        let s = Schema::object("Root")
            .nested(
                "db",
                Schema::object("Db").field("url", Field::string().optional()),
            )
            .build();
        match &s.fields[0].field {
            Shape::Object(inner) => {
                assert_eq!(inner.name, "Db");
                assert_eq!(inner.fields.len(), 1);
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn builder_handles_strict_override() {
        let s = Schema::object("Top").strict(false).build();
        assert_eq!(s.strict, Some(false));
    }

    #[test]
    fn enum_of_collects_values() {
        let f = Field::enum_of(["debug", "info"]).build();
        match &f {
            Shape::Leaf(leaf) => match &leaf.ty {
                LeafType::Enum { values } => {
                    assert_eq!(values.len(), 2);
                    assert_eq!(values[0], Value::String("debug".into()));
                }
                other => panic!("expected Enum, got {other:?}"),
            },
            other => panic!("expected Leaf, got {other:?}"),
        }
    }

    fn unbounded_integer() -> LeafType {
        LeafType::Integer {
            min: None,
            max: None,
        }
    }

    #[test]
    fn leaf_type_check_accepts_matching_primitives() {
        assert!(LeafType::String.check(&Value::String("x".into())).is_ok());
        assert!(unbounded_integer().check(&Value::Integer(1)).is_ok());
        assert!(LeafType::Bool.check(&Value::Boolean(true)).is_ok());
    }

    #[test]
    fn leaf_type_check_rejects_mismatched_type() {
        let err = unbounded_integer()
            .check(&Value::String("nope".into()))
            .unwrap_err();
        assert!(err.contains("expected integer"));
        assert!(err.contains("got string"));
    }

    #[test]
    fn leaf_type_check_float_accepts_integer_value() {
        // serde accepts `timeout = 5` for an f64 field; the schema check
        // must not be stricter than the deserializer.
        assert!(LeafType::Float.check(&Value::Integer(5)).is_ok());
        assert!(LeafType::Float.check(&Value::Float(5.0)).is_ok());
        let err = LeafType::Float
            .check(&Value::String("5".into()))
            .unwrap_err();
        assert!(err.contains("expected float"));
    }

    #[test]
    fn leaf_type_check_integer_enforces_bounds() {
        let u8_like = LeafType::Integer {
            min: Some(0),
            max: Some(255),
        };
        assert!(u8_like.check(&Value::Integer(0)).is_ok());
        assert!(u8_like.check(&Value::Integer(255)).is_ok());
        let err = u8_like.check(&Value::Integer(300)).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
        assert!(err.contains("0..=255"), "{err}");
        let err = u8_like.check(&Value::Integer(-1)).unwrap_err();
        assert!(err.contains("out of range"), "{err}");

        let non_negative = LeafType::Integer {
            min: Some(0),
            max: None,
        };
        assert!(non_negative.check(&Value::Integer(i64::MAX)).is_ok());
        let err = non_negative.check(&Value::Integer(-5)).unwrap_err();
        assert!(err.contains(">= 0"), "{err}");
    }

    #[test]
    fn leaf_type_check_enum_accepts_known_value() {
        let e = LeafType::Enum {
            values: vec!["info".into(), "warn".into()],
        };
        assert!(e.check(&Value::String("info".into())).is_ok());
    }

    #[test]
    fn leaf_type_check_enum_rejects_unknown_value() {
        let e = LeafType::Enum {
            values: vec!["info".into(), "warn".into()],
        };
        let err = e.check(&Value::String("garbage".into())).unwrap_err();
        assert!(err.contains("not in allowed set"));
        assert!(err.contains("\"info\""));
        assert!(err.contains("\"warn\""));
    }

    #[test]
    fn leaf_type_value_accepts_any_shape() {
        let v = LeafType::Value;
        assert!(v.check(&Value::String("warn".into())).is_ok());
        assert!(v.check(&Value::Integer(42)).is_ok());
        assert!(v.check(&Value::Boolean(true)).is_ok());
        assert!(
            v.check(&Value::Array(vec![
                Value::String("warn".into()),
                Value::Map({
                    let mut t = crate::value::Map::new();
                    t.insert("max_columns".into(), Value::Integer(80));
                    t
                }),
            ]))
            .is_ok()
        );
        assert!(v.check(&Value::Map(crate::value::Map::new())).is_ok());
    }

    #[test]
    fn field_value_constructs_value_leaf() {
        let f = Field::value().build();
        match f {
            Shape::Leaf(leaf) => assert!(matches!(leaf.ty, LeafType::Value)),
            other => panic!("expected Leaf, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "integer_in min")]
    fn integer_in_rejects_min_greater_than_max() {
        let _ = Field::integer_in(Some(10), Some(1));
    }

    #[test]
    fn integer_in_accepts_open_and_equal_ends() {
        let _ = Field::integer_in(Some(0), None);
        let _ = Field::integer_in(None, Some(10));
        let _ = Field::integer_in(Some(5), Some(5));
        let _ = Field::integer_in(None, None);
    }

    #[test]
    fn shape_check_array_recurses() {
        let arr = Shape::from(Field::array_of_type(unbounded_integer()));
        let good = Value::Array(vec![Value::Integer(1), Value::Integer(2)]);
        assert!(arr.check_value(&good).is_ok());

        let bad = Value::Array(vec![Value::Integer(1), Value::String("oops".into())]);
        let err = arr.check_value(&bad).unwrap_err();
        assert!(err.contains("array[1]"));
        assert!(err.contains("expected integer"));
    }

    #[test]
    #[should_panic(expected = "contains '.'")]
    fn field_name_with_dot_panics() {
        let _ = Schema::object("Top").field("a.b", Field::string()).build();
    }

    #[test]
    #[should_panic(expected = "contains '['")]
    fn field_name_with_open_bracket_panics() {
        let _ = Schema::object("Top").field("a[0]", Field::string()).build();
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn empty_field_name_panics() {
        let _ = Schema::object("Top").field("", Field::string()).build();
    }

    #[test]
    #[should_panic(expected = "duplicate field name")]
    fn duplicate_field_name_panics() {
        let _ = Schema::object("Top")
            .field("a", Field::string())
            .field("a", Field::integer())
            .build();
    }

    #[test]
    fn nested_and_array_of_share_the_same_validation() {
        // Sanity: validator fires for `nested` / `array_of` too, not just
        // `field`. Builds one of each cleanly, then asserts a duplicate
        // collision across categories also trips the same panic.
        let _ = Schema::object("Top")
            .nested("a", Schema::object("A"))
            .array_of("b", Schema::object("B"))
            .build();
        let result = std::panic::catch_unwind(|| {
            Schema::object("Top")
                .field("a", Field::string())
                .nested("a", Schema::object("Dup")) // dup across kinds
                .build()
        });
        assert!(result.is_err(), "duplicate across leaf/nested must panic");
    }

    #[test]
    fn shape_check_map_recurses() {
        let map = Shape::from(Field::map_of(unbounded_integer()));
        let mut t = crate::value::Map::new();
        t.insert("a".into(), Value::Integer(1));
        assert!(map.check_value(&Value::Map(t.clone())).is_ok());

        t.insert("b".into(), Value::String("oops".into()));
        let err = map.check_value(&Value::Map(t)).unwrap_err();
        assert!(err.contains("map[b]"));
    }

    fn object(name: &str) -> Schema {
        Schema::object(name).field("x", Field::string()).build()
    }

    #[test]
    fn shape_object_from_schema_is_a_legal_document_root() {
        let shape = Shape::from(object("App"));
        assert!(matches!(shape, Shape::Object(_)));
        assert!(shape.is_legal_document_root());
        shape.require_document_root();
        let _ = crate::Clapfig::builder(object("App"));
    }

    #[test]
    fn root_may_be_a_map() {
        let root = Shape::from(
            Shape::map("blocks", object("Block"))
                .doc("named instances")
                .build(),
        );
        match &root {
            Shape::Map(map) => {
                assert_eq!(map.name, "blocks");
                assert_eq!(map.doc, vec!["named instances".to_string()]);
                assert!(matches!(map.item.as_ref(), Shape::Object(_)));
            }
            other => panic!("expected Map, got {other:?}"),
        }
        assert!(root.is_legal_document_root());
        root.require_document_root();
    }

    #[test]
    fn root_may_be_tagged() {
        let root = Shape::from(
            Shape::tagged("Block", "kind")
                .doc("internally tagged block")
                .variant("rust", object("Rust"))
                .variant("payload", object("Payload"))
                .build(),
        );
        match &root {
            Shape::Tagged(tagged) => {
                assert_eq!(tagged.name, "Block");
                assert_eq!(tagged.tag, "kind");
                assert_eq!(tagged.variants.len(), 2);
                assert_eq!(tagged.variants[0].discriminator, "rust");
                assert_eq!(tagged.variants[0].schema.name, "Rust");
                assert_eq!(tagged.variants[1].discriminator, "payload");
            }
            other => panic!("expected Tagged, got {other:?}"),
        }
        assert!(root.is_legal_document_root());
        root.require_document_root();
    }

    #[test]
    fn tagged_has_closed_unique_non_empty_discriminator_set() {
        let tagged = Shape::tagged("Block", "kind")
            .variant("rust", object("Rust"))
            .variant("payload", object("Payload"))
            .build();
        let names: Vec<&str> = tagged
            .variants
            .iter()
            .map(|v| v.discriminator.as_str())
            .collect();
        assert_eq!(names, ["rust", "payload"]);
        assert!(!names.iter().any(|n| n.is_empty()));
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn tagged_variants_are_objects() {
        let tagged = Shape::tagged("Block", "kind")
            .variant("off", Schema::object("Off").build())
            .variant("rust", object("Rust"))
            .build();
        // A unit variant is the empty object (tag-only on the wire).
        assert!(tagged.variants[0].schema.fields.is_empty());
        assert_eq!(tagged.variants[1].schema.fields.len(), 1);
        for variant in &tagged.variants {
            // Stored payload is Schema — the Object constructor — not an
            // arbitrary Shape.
            let _object: &Schema = &variant.schema;
        }
    }

    #[test]
    fn leaf_and_array_construct_as_nested_shapes() {
        let leaf = Shape::leaf(LeafType::String);
        assert!(matches!(leaf, Shape::Leaf(_)));
        assert!(!leaf.is_legal_document_root());

        let array = Shape::from(Shape::array("flags", Field::string()).build());
        assert!(matches!(array, Shape::Array(_)));
        assert!(!array.is_legal_document_root());

        // Nested: a map of arrays of leaves is a constructible tree.
        let nested = Shape::from(Shape::map("groups", array).build());
        assert!(nested.is_legal_document_root());
    }

    #[test]
    fn tagged_discriminator_values_are_not_path_segments() {
        // Discriminators are closed enum values: serde-valid spellings
        // with `.` / `[` / `]` must construct, matching derive.
        let tagged = Shape::tagged("Block", "kind")
            .variant("rust.v2", Schema::object("RustV2").build())
            .variant("[legacy]", Schema::object("Legacy").build())
            .build();
        let names: Vec<&str> = tagged
            .variants
            .iter()
            .map(|v| v.discriminator.as_str())
            .collect();
        assert_eq!(names, ["rust.v2", "[legacy]"]);
    }

    #[test]
    fn map_of_tagged_objects_constructs() {
        let tagged = Shape::from(
            Shape::tagged("Block", "kind")
                .variant("rust", object("Rust"))
                .build(),
        );
        let root = Shape::from(Shape::map("block", tagged).build());
        match root {
            Shape::Map(map) => match map.item.as_ref() {
                Shape::Tagged(t) => assert_eq!(t.tag, "kind"),
                other => panic!("expected Tagged item, got {other:?}"),
            },
            other => panic!("expected Map, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "a Leaf is not a legal document root")]
    fn leaf_is_not_a_legal_document_root() {
        Shape::leaf(LeafType::String).require_document_root();
    }

    #[test]
    #[should_panic(expected = "an Array is not a legal document root")]
    fn array_is_not_a_legal_document_root() {
        Shape::from(Shape::array("xs", Field::integer()).build()).require_document_root();
    }

    #[test]
    #[should_panic(expected = "a Leaf is not a legal document root")]
    fn clapfig_builder_rejects_leaf_document_root() {
        let _ = crate::Clapfig::builder(Shape::leaf(LeafType::String));
    }

    #[test]
    #[should_panic(expected = "an Array is not a legal document root")]
    fn clapfig_builder_rejects_array_document_root() {
        let _ = crate::Clapfig::builder(Shape::array("xs", Field::string()).build());
    }

    #[test]
    fn clapfig_builder_accepts_root_map() {
        let _ = crate::Clapfig::builder(Shape::map("blocks", object("Block")).build());
    }

    #[test]
    fn clapfig_builder_accepts_root_tagged() {
        let _ = crate::Clapfig::builder(
            Shape::tagged("Block", "kind")
                .variant("rust", object("Rust"))
                .build(),
        );
    }

    #[test]
    #[should_panic(expected = "must have at least one variant")]
    fn tagged_rejects_empty_union() {
        let _ = Shape::tagged("Block", "kind").build();
    }

    #[test]
    #[should_panic(expected = "discriminator field name must not be empty")]
    fn tagged_rejects_empty_tag_name() {
        let _ = Shape::tagged("Block", "");
    }

    #[test]
    #[should_panic(expected = "discriminator must not be empty")]
    fn tagged_rejects_empty_discriminator() {
        let _ = Shape::tagged("Block", "kind")
            .variant("", object("Rust"))
            .build();
    }

    #[test]
    #[should_panic(expected = "duplicate tagged discriminator")]
    fn tagged_rejects_duplicate_discriminators() {
        let _ = Shape::tagged("Block", "kind")
            .variant("rust", object("Rust"))
            .variant("rust", object("AlsoRust"))
            .build();
    }

    #[test]
    #[should_panic(expected = "must be an object, got Leaf")]
    fn tagged_rejects_non_object_leaf_variant() {
        let _ = Shape::tagged("Block", "kind")
            .variant("rust", Shape::leaf(LeafType::String))
            .build();
    }

    #[test]
    #[should_panic(expected = "must be an object, got Map")]
    fn tagged_rejects_non_object_map_variant() {
        let _ = Shape::tagged("Block", "kind")
            .variant("rust", Shape::map("inner", object("Inner")).build())
            .build();
    }

    #[test]
    #[should_panic(expected = "must be an object, got Array")]
    fn tagged_rejects_non_object_array_variant() {
        let _ = Shape::tagged("Block", "kind")
            .variant("rust", Shape::array("xs", Field::string()).build())
            .build();
    }

    #[test]
    #[should_panic(expected = "must be an object, got Tagged")]
    fn tagged_rejects_nested_tagged_variant() {
        let inner = Shape::tagged("Inner", "k")
            .variant("a", object("A"))
            .build();
        let _ = Shape::tagged("Block", "kind")
            .variant("rust", inner)
            .build();
    }

    #[test]
    #[should_panic(expected = "contains '.'")]
    fn tagged_rejects_tag_with_dot() {
        let _ = Shape::tagged("Block", "k.ind");
    }

    #[test]
    #[should_panic(expected = "contains '['")]
    fn tagged_rejects_tag_with_open_bracket() {
        let _ = Shape::tagged("Block", "k[ind]");
    }

    #[test]
    #[should_panic(expected = "contains ']'")]
    fn tagged_rejects_tag_with_close_bracket() {
        let _ = Shape::tagged("Block", "k]ind");
    }

    #[test]
    #[should_panic(expected = "must not declare a field named \"kind\"")]
    fn tagged_rejects_variant_field_named_as_tag() {
        let _ = Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("kind", Field::string())
                    .build(),
            )
            .build();
    }

    #[test]
    fn shape_builders_convert_without_build() {
        let map: Shape = Shape::map("blocks", object("Block")).into();
        assert!(matches!(map, Shape::Map(_)));
        let array: Shape = Shape::array("xs", Field::string()).into();
        assert!(matches!(array, Shape::Array(_)));
        let tagged: Shape = Shape::tagged("Block", "kind")
            .variant("rust", object("Rust"))
            .into();
        assert!(matches!(tagged, Shape::Tagged(_)));
    }

    fn block_union() -> TaggedShape {
        Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("mount", Field::string())
                    .field("crate_path", Field::string())
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field("mount", Field::string())
                    .field("artifact", Field::string())
                    .build(),
            )
            .build()
    }

    #[test]
    fn resolve_key_classifies_tag_absent_every_partial() {
        let tagged = block_union();
        assert!(matches!(tagged.resolve_key("kind"), KeyAcrossVariants::Tag));
        assert!(matches!(
            tagged.resolve_key("nope"),
            KeyAcrossVariants::Absent
        ));
        match tagged.resolve_key("mount") {
            KeyAcrossVariants::Every(shapes) => assert_eq!(shapes.len(), 2),
            other => panic!("expected Every, got {other:?}"),
        }
        match tagged.resolve_key("crate_path") {
            KeyAcrossVariants::Partial(shapes) => assert_eq!(shapes.len(), 1),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn structurally_agrees_objects_ignore_fields() {
        let a = Shape::from(Schema::object("A").field("x", Field::string()).build());
        let b = Shape::from(
            Schema::object("B")
                .field("y", Field::integer())
                .field("z", Field::boolean())
                .build(),
        );
        assert!(
            a.structurally_agrees_with(&b),
            "Object/Object agrees regardless of fields"
        );
    }

    #[test]
    fn structurally_agrees_tagged_compares_tag_only() {
        let same_tag = Shape::from(
            Shape::tagged("A", "kind")
                .variant("rust", Schema::object("Rust").build())
                .build(),
        );
        let same_tag_other_variants = Shape::from(
            Shape::tagged("B", "kind")
                .variant(
                    "payload",
                    Schema::object("Payload")
                        .field("artifact", Field::string())
                        .build(),
                )
                .build(),
        );
        let other_tag = Shape::from(
            Shape::tagged("C", "type")
                .variant("rust", Schema::object("Rust").build())
                .build(),
        );
        assert!(
            same_tag.structurally_agrees_with(&same_tag_other_variants),
            "Tagged/Tagged compares the tag only, not variants"
        );
        assert!(
            !same_tag.structurally_agrees_with(&other_tag),
            "different tags must not agree"
        );
    }

    #[test]
    fn structurally_agrees_leaves_and_containers() {
        let string = Shape::from(Field::string());
        let other_string = Shape::from(Field::string());
        let integer = Shape::from(Field::integer());
        let bounded = Shape::from(Field::integer_in(Some(0), Some(255)));
        assert!(string.structurally_agrees_with(&other_string));
        assert!(!string.structurally_agrees_with(&integer));
        assert!(!integer.structurally_agrees_with(&bounded));

        let a = Shape::from(Field::array_of_type(Field::string()));
        let b = Shape::from(Field::array_of_type(Field::string()));
        let c = Shape::from(Field::array_of_type(Field::integer()));
        assert!(a.structurally_agrees_with(&b));
        assert!(!a.structurally_agrees_with(&c));

        let map_s = Shape::from(Field::map_of(Field::string()));
        let map_i = Shape::from(Field::map_of(Field::integer()));
        assert!(map_s.structurally_agrees_with(&Shape::from(Field::map_of(Field::string()))));
        assert!(!map_s.structurally_agrees_with(&map_i));

        assert!(!string.structurally_agrees_with(&Shape::from(object("App"))));
    }
}
