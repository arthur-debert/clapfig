//! Runtime-defined schemas: owned `Schema` / `Field` / `LeafType` types and a
//! fluent builder, for callers without a compile-time `confique::Config`
//! struct.
//!
//! Pairs with a crate-private schema abstraction introduced in Phase 1
//! (a borrowed `SchemaRef` view). The runtime-side `Schema` is converted
//! to that view internally, and every consumer that already walks the
//! borrowed view — strict-mode validation, doc lookup, valid-key
//! enumeration, JSON Schema generation, template generation, persistence
//! validation — works over either source without a recompile-time
//! struct.
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

use toml::Value;

/// Owned, runtime-defined schema for a config node.
///
/// Constructed via [`Schema::object`] and the fluent builder, or directly
/// as a plain data struct. The clapfig resolve pipeline borrows from
/// this internally — callers normally only need to build it and hand it
/// to [`Clapfig::runtime`](crate::Clapfig::runtime).
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
    /// Start building a schema with the given object name (analogous to the
    /// struct name in the static path).
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

/// A schema field — leaf scalar / array, nested object, or array-of-objects.
#[derive(Debug, Clone)]
pub enum Field {
    Leaf(Leaf),
    /// A single nested object — TOML `[section]`.
    Nested(Schema),
    /// An array of nested objects — TOML `[[plugins]]`.
    ArrayOf(Schema),
}

impl Field {
    /// Start a leaf builder for a string value.
    pub fn string() -> FieldBuilder {
        FieldBuilder::new(LeafType::String)
    }

    /// Start a leaf builder for an integer value.
    pub fn integer() -> FieldBuilder {
        FieldBuilder::new(LeafType::Integer)
    }

    /// Start a leaf builder for a floating-point value.
    pub fn float() -> FieldBuilder {
        FieldBuilder::new(LeafType::Float)
    }

    /// Start a leaf builder for a boolean value.
    pub fn boolean() -> FieldBuilder {
        FieldBuilder::new(LeafType::Bool)
    }

    /// Start a leaf builder for a TOML datetime value.
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
    /// Each `value` must be representable as a TOML primitive (string,
    /// integer, float, or bool). At load time, a merged value not in this
    /// set produces [`ClapfigError::InvalidValue`](crate::error::ClapfigError::InvalidValue).
    pub fn enum_of<V: Into<Value>, I: IntoIterator<Item = V>>(values: I) -> FieldBuilder {
        let values: Vec<Value> = values.into_iter().map(Into::into).collect();
        FieldBuilder::new(LeafType::Enum { values })
    }

    /// Start a leaf builder that accepts any TOML value.
    ///
    /// Escape hatch for keys whose value can take multiple incompatible
    /// shapes (e.g. a bare string *or* an array, like serde's
    /// `#[serde(untagged)]` enums). Clapfig will not type-check the value
    /// at this layer; the caller is responsible for any further validation
    /// or deserialization, typically inside a `post_validate` hook or
    /// after `RuntimeResolver::resolve`.
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
    Integer,
    Float,
    Bool,
    /// TOML datetime (offset, local-datetime, local-date, local-time).
    DateTime,
    /// Homogeneous array. The boxed `LeafType` is the element type.
    Array(Box<LeafType>),
    /// String-keyed map with homogeneous values. The boxed `LeafType` is the
    /// value type.
    Map(Box<LeafType>),
    /// Constrained value: must equal one of the listed TOML values.
    Enum {
        values: Vec<Value>,
    },
    /// Accept any TOML value (scalar, array, table). Clapfig performs no
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
            LeafType::Integer => "integer",
            LeafType::Float => "float",
            LeafType::Bool => "bool",
            LeafType::DateTime => "datetime",
            LeafType::Array(_) => "array",
            LeafType::Map(_) => "map",
            LeafType::Enum { .. } => "enum",
            LeafType::Value => "value",
        }
    }

    /// Check whether a `toml::Value` is shape-compatible with this leaf type.
    ///
    /// Containers (`Array`, `Map`) recurse into their elements. `Enum` checks
    /// literal equality against the allowed-value set. Returns `Ok(())` on
    /// match; on mismatch, returns a human-readable reason suitable for
    /// `ClapfigError::InvalidValue::reason`.
    pub(crate) fn check(&self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (LeafType::String, Value::String(_)) => Ok(()),
            (LeafType::Integer, Value::Integer(_)) => Ok(()),
            (LeafType::Float, Value::Float(_)) => Ok(()),
            (LeafType::Bool, Value::Boolean(_)) => Ok(()),
            (LeafType::DateTime, Value::Datetime(_)) => Ok(()),
            (LeafType::Array(elem), Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    elem.check(item).map_err(|e| format!("array[{i}]: {e}"))?;
                }
                Ok(())
            }
            (LeafType::Map(elem), Value::Table(table)) => {
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
                        .map(format_toml_value)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    Err(format!(
                        "value {} is not in allowed set: {listed}",
                        format_toml_value(v)
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

/// Pretty-print a `toml::Value` for error messages.
fn format_toml_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(_) => "<array>".into(),
        Value::Table(_) => "<table>".into(),
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "table",
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

    #[test]
    fn leaf_type_check_accepts_matching_primitives() {
        assert!(LeafType::String.check(&Value::String("x".into())).is_ok());
        assert!(LeafType::Integer.check(&Value::Integer(1)).is_ok());
        assert!(LeafType::Bool.check(&Value::Boolean(true)).is_ok());
    }

    #[test]
    fn leaf_type_check_rejects_mismatched_type() {
        let err = LeafType::Integer
            .check(&Value::String("nope".into()))
            .unwrap_err();
        assert!(err.contains("expected integer"));
        assert!(err.contains("got string"));
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
                Value::Table({
                    let mut t = toml::Table::new();
                    t.insert("max_columns".into(), Value::Integer(80));
                    t
                }),
            ]))
            .is_ok()
        );
        assert!(v.check(&Value::Table(toml::Table::new())).is_ok());
    }

    #[test]
    fn field_value_constructs_value_leaf() {
        let f = Field::value().build();
        assert!(matches!(f.ty, LeafType::Value));
    }

    #[test]
    fn leaf_type_check_array_recurses() {
        let arr = LeafType::Array(Box::new(LeafType::Integer));
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
        let map = LeafType::Map(Box::new(LeafType::Integer));
        let mut t = toml::map::Map::new();
        t.insert("a".into(), Value::Integer(1));
        assert!(map.check(&Value::Table(t.clone())).is_ok());

        t.insert("b".into(), Value::String("oops".into()));
        let err = map.check(&Value::Table(t)).unwrap_err();
        assert!(err.contains("map[b]"));
    }
}
