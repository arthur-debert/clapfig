//! Runtime-defined schemas: owned [`Shape`] / [`Schema`] / [`Field`] /
//! [`LeafType`] types and a fluent builder, for callers without a compile-time
//! `#[derive(clapfig::Schema)]` struct.
//!
//! [`Shape`] is the schema node (ADR-0010, `docs/spec/shape-algebra.md`):
//! leaf, named-field object, homogeneous map, homogeneous array, or
//! internally tagged union. [`Schema`] is the named-field **object**
//! constructor (`Schema::object`), not the node, and is not renamed to
//! `Object`. [`clapfig::Schema`](crate::Schema) is the derive trait.
//!
//! Walkers still take `&Schema` today; SHP01-WS02 switches them to
//! [`Shape`]. Until then, [`Clapfig::builder`](crate::Clapfig::builder)
//! accepts `impl Into<Shape>` so object-root callers keep passing a
//! [`Schema`]. Root Map and Tagged shapes construct; loading them is a
//! loud stub, not a silent object walk.
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

use crate::value::Value;

/// Named-field object — the [`Shape::Object`] constructor, not the schema
/// node (ADR-0010).
///
/// Constructed via [`Schema::object`] and the fluent builder, or directly
/// as a plain data struct. The clapfig resolve pipeline still walks
/// `&Schema` today (SHP01-WS02 switches walkers to [`Shape`]). Object-root
/// callers hand this to [`Clapfig::builder`](crate::Clapfig::builder),
/// which takes `impl Into<Shape>`.
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

    /// Add a leaf field.
    ///
    /// `name` is treated as a single TOML key and cannot contain `.` (the
    /// dotted-path separator), `[`, or `]` (array-index syntax), and cannot
    /// be empty. Violating this panics — the cost of constructing a schema
    /// with an ambiguous segment now is strictly less than the cost of
    /// debugging silent `KeyNotFound`s at every consumer (the resolve
    /// pipeline, persist, cascade lookup) down the line.
    pub fn field(mut self, name: impl Into<String>, field: FieldBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        self.schema.fields.push(NamedField {
            name,
            field: Field::Leaf(field.build()),
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
            field: Field::Nested(child.build()),
        });
        self
    }

    /// Add an array of nested objects (TOML `[[name]]`). Same `name`
    /// constraints as [`field`](Self::field).
    pub fn array_of(mut self, name: impl Into<String>, item: SchemaBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        self.schema.fields.push(NamedField {
            name,
            field: Field::ArrayOf(item.build()),
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
    /// rather than the map level for strictness purposes.
    pub fn map_of(mut self, name: impl Into<String>, item: SchemaBuilder) -> Self {
        let name = name.into();
        validate_field_name(&self.schema, &name);
        self.schema.fields.push(NamedField {
            name,
            field: Field::MapOf(item.build()),
        });
        self
    }

    /// Finalize the builder into a [`Schema`].
    pub fn build(self) -> Schema {
        self.schema
    }
}

/// A named field on a [`Schema`].
#[derive(Debug, Clone)]
pub struct NamedField {
    pub name: String,
    pub field: Field,
}

/// A schema field — leaf scalar / array, nested object, array-of-objects,
/// or map-of-objects.
#[derive(Debug, Clone)]
pub enum Field {
    Leaf(Leaf),
    /// A single nested object — TOML `[section]`.
    Nested(Schema),
    /// An array of nested objects — TOML `[[plugins]]`. Deserializes to
    /// `Vec<T>` where `T` is a struct deriving [`Schema`](crate::Schema);
    /// an absent array loads as the empty `Vec`.
    ArrayOf(Schema),
    /// A string-keyed map of nested objects — TOML `[plugins.<key>]` with
    /// arbitrary `<key>` names. Sibling of [`ArrayOf`](Field::ArrayOf) for
    /// the dual shape: keyed map of objects instead of indexed array of
    /// objects. Deserializes to `BTreeMap<String, T>` / `HashMap<String, T>`
    /// where `T` is a struct deriving [`Schema`](crate::Schema).
    MapOf(Schema),
}

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

    /// Start a leaf builder for a homogeneous array.
    pub fn array_of_type(item: LeafType) -> FieldBuilder {
        FieldBuilder::new(LeafType::Array(Box::new(item)))
    }

    /// Start a leaf builder for a string-keyed map with homogeneous values.
    pub fn map_of(value: LeafType) -> FieldBuilder {
        FieldBuilder::new(LeafType::Map(Box::new(value)))
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
    /// Homogeneous array. The boxed `LeafType` is the element type.
    Array(Box<LeafType>),
    /// String-keyed map with homogeneous values. The boxed `LeafType` is the
    /// value type.
    Map(Box<LeafType>),
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
            LeafType::Array(_) => "array",
            LeafType::Map(_) => "map",
            LeafType::Enum { .. } => "enum",
            LeafType::Value => "value",
        }
    }

    /// Check whether a [`Value`] is shape-compatible with this leaf type.
    ///
    /// Containers (`Array`, `Map`) recurse into their elements. `Enum` checks
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
            (LeafType::Array(elem), Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    elem.check(item).map_err(|e| format!("array[{i}]: {e}"))?;
                }
                Ok(())
            }
            (LeafType::Map(elem), Value::Map(table)) => {
                for (k, v) in table {
                    elem.check(v).map_err(|e| format!("map[{k}]: {e}"))?;
                }
                Ok(())
            }
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

/// Fluent builder for a [`Leaf`] field.
#[derive(Debug, Clone)]
pub struct FieldBuilder {
    leaf: Leaf,
}

impl FieldBuilder {
    fn new(ty: LeafType) -> Self {
        Self {
            leaf: Leaf {
                doc: Vec::new(),
                ty,
                default: None,
                optional: false,
                env: None,
            },
        }
    }

    /// Append a doc-comment line.
    pub fn doc(mut self, line: impl Into<String>) -> Self {
        self.leaf.doc.push(line.into());
        self
    }

    /// Set the default value injected when no layer supplies one.
    pub fn default<V: Into<Value>>(mut self, value: V) -> Self {
        self.leaf.default = Some(value.into());
        self
    }

    /// Mark this field optional — absence after merge is accepted.
    pub fn optional(mut self) -> Self {
        self.leaf.optional = true;
        self
    }

    /// Override the env-var name for this field. Without this, the env layer
    /// derives a name from the field path.
    pub fn env(mut self, name: impl Into<String>) -> Self {
        self.leaf.env = Some(name.into());
        self
    }

    pub(crate) fn build(self) -> Leaf {
        self.leaf
    }
}

/// A schema node: leaf, named-field object, homogeneous map, homogeneous
/// array, or internally tagged union.
///
/// This is the node walkers will take (SHP01-WS02). [`Schema`] remains the
/// named-field object — the [`Shape::Object`] constructor — not this enum
/// and not renamed to `Object` (ADR-0010). An object's field value is a
/// [`Shape`]; [`Field`] is not a second node in this contract (the public
/// collapse of `Field` / `LeafType::Map` / `LeafType::Array` is SHP01-WS02).
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
    /// document root; load of a root map is SHP01-WS03.
    Map(MapShape),
    /// Homogeneous array. Item is any shape. Not a legal document root.
    Array(ArrayShape),
    /// Internally tagged union of objects, selected by a discriminator
    /// field. A legal document root; tagged walk is SHP01-WS04.
    Tagged(TaggedShape),
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
            },
        }
    }

    /// Start an internally tagged union. A legal document root.
    ///
    /// `name` is the node name (the enum type name). `tag` is the
    /// discriminator field (`serde(tag = ...)`), reserved on every variant
    /// object and not itself a field of the variant. Panics if `tag` is
    /// empty. Call [`TaggedShapeBuilder::variant`] at least once before
    /// [`build`](TaggedShapeBuilder::build).
    pub fn tagged(name: impl Into<String>, tag: impl Into<String>) -> TaggedShapeBuilder {
        let tag = tag.into();
        assert!(
            !tag.is_empty(),
            "clapfig: tagged discriminator field name must not be empty"
        );
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
        Shape::Leaf(builder.build())
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

/// Homogeneous unordered map: string keys, item is any [`Shape`].
///
/// Node header (`name` / `doc` / `strict`) parallels [`Schema`]. A legal
/// document root; walkers consume this in SHP01-WS02 / WS03.
#[derive(Debug, Clone)]
pub struct MapShape {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,
    pub item: Box<Shape>,
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
/// Not a legal document root. Node header parallels [`Schema`].
#[derive(Debug, Clone)]
pub struct ArrayShape {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,
    pub item: Box<Shape>,
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
/// A legal document root; tagged walk is SHP01-WS04.
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
    /// within this union.
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

/// Reject field names that would confuse every downstream consumer
/// (resolve, persist, cascade lookup) the moment they're constructed.
///
/// - `.` would be re-parsed as a dotted-path separator (`Schema::field("a.b", ...)`
///   would never be findable via `find_field(schema, "a")`).
/// - `[` / `]` collide with the array-index syntax the cascade walker
///   strips out.
/// - Empty names produce confusing `KeyNotFound` errors with a blank
///   token.
/// - Duplicate names within one schema make `find_field` order-dependent
///   and `valid_keys` collide.
fn validate_field_name(schema: &Schema, name: &str) {
    assert!(!name.is_empty(), "clapfig: field name must not be empty");
    assert!(
        !name.contains('.'),
        "clapfig: field name {name:?} contains '.', which conflicts with the dotted-path separator"
    );
    assert!(
        !name.contains('['),
        "clapfig: field name {name:?} contains '[', which conflicts with array-index syntax"
    );
    assert!(
        !name.contains(']'),
        "clapfig: field name {name:?} contains ']', which conflicts with array-index syntax"
    );
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
        assert!(matches!(s.fields[0].field, Field::Leaf(_)));
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
            Field::Nested(inner) => {
                assert_eq!(inner.name, "Db");
                assert_eq!(inner.fields.len(), 1);
            }
            other => panic!("expected Nested, got {other:?}"),
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
        match &f.ty {
            LeafType::Enum { values } => {
                assert_eq!(values.len(), 2);
                assert_eq!(values[0], Value::String("debug".into()));
            }
            other => panic!("expected Enum, got {other:?}"),
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
        assert!(matches!(f.ty, LeafType::Value));
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
    fn leaf_type_check_array_recurses() {
        let arr = LeafType::Array(Box::new(unbounded_integer()));
        let good = Value::Array(vec![Value::Integer(1), Value::Integer(2)]);
        assert!(arr.check(&good).is_ok());

        let bad = Value::Array(vec![Value::Integer(1), Value::String("oops".into())]);
        let err = arr.check(&bad).unwrap_err();
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
    fn leaf_type_check_map_recurses() {
        let map = LeafType::Map(Box::new(unbounded_integer()));
        let mut t = crate::value::Map::new();
        t.insert("a".into(), Value::Integer(1));
        assert!(map.check(&Value::Map(t.clone())).is_ok());

        t.insert("b".into(), Value::String("oops".into()));
        let err = map.check(&Value::Map(t)).unwrap_err();
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
    #[should_panic(expected = "root Map and Tagged")]
    fn clapfig_builder_does_not_silently_load_root_map() {
        let _ = crate::Clapfig::builder(Shape::map("blocks", object("Block")).build());
    }

    #[test]
    #[should_panic(expected = "root Map and Tagged")]
    fn clapfig_builder_does_not_silently_load_root_tagged() {
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
}
