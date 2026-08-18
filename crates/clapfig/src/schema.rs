//! JSON Schema generation from a config schema.
//!
//! Entry point is [`generate_schema`]: it walks a
//! [`runtime::Schema`](crate::runtime::Schema) — built by the runtime
//! builder or emitted by `#[derive(clapfig::Schema)]` — and produces a JSON
//! Schema document. Useful for auto-generating UI editors, external
//! validation tools, or IDE integrations.
//!
//! Internally the walker consumes a crate-private `SchemaRef` view so the
//! same generator serves both entry points without a separate code path.
//!
//! # What is in the schema
//!
//! - **Structure**: every nested config object becomes a JSON `object` with
//!   `properties`; non-optional fields are listed in `required`.
//! - **Docs**: schema and field doc lines become `description`.
//! - **Types**: emitted from each leaf's declared
//!   [`LeafType`] — including leaves without
//!   defaults. String → `"string"`, integer → `"integer"`, float →
//!   `"number"`, bool → `"boolean"`, array → `"array"`, map → `"object"`.
//! - **Defaults**: the literal default value (when present) is emitted as
//!   `default` on the property.
//! - **Enums**: `Enum { values }` leaves emit `enum: [...]` alongside the
//!   primitive type implied by the value set.
//! - **Env vars**: when a field maps to an env var, the name is attached as
//!   the non-standard `x-env` extension.
//! - **JSON comment keys**: every object allowlists the `^//` key pattern
//!   (`patternProperties`) alongside `additionalProperties: false`. This is
//!   for third-party validators only — editors validating a documented JSON
//!   template against this schema directly. Clapfig's own validation never
//!   sees the keys: the JSON adapter strips the reserved `//` namespace at
//!   parse time (ADR-0002).
//!
//! # Example
//!
//! ```ignore
//! use clapfig::schema;
//!
//! let value = schema::generate_schema(MyConfig::schema());
//! println!("{}", serde_json::to_string_pretty(&value).unwrap());
//! ```

use serde_json::{Map, Value, json};

use crate::runtime::LeafType;
use crate::spec::{FieldKindRef, FieldRef, LeafRef, SchemaRef};
use crate::value::Value as ConfigValue;

/// JSON Schema dialect emitted in the root `$schema` field.
const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The reserved JSON comment-key namespace (ADR-0002), allowlisted on
/// every object via `patternProperties` so documented JSON templates
/// validate against the schema in third-party tooling. Keys matching the
/// pattern escape `additionalProperties: false` (and, on map-of objects,
/// the entry schema).
const COMMENT_KEY_PATTERN: &str = "^//";

/// The `patternProperties` object allowlisting [`COMMENT_KEY_PATTERN`]
/// (the empty schema `{}` accepts any comment value shape).
fn comment_key_allowlist() -> Value {
    json!({ COMMENT_KEY_PATTERN: {} })
}

/// Generate a JSON Schema document from a config schema.
///
/// Works for both entry points: pass the schema handed to
/// [`Clapfig::runtime`](crate::Clapfig::runtime), or a derive-emitted
/// schema via `C::schema()`.
///
/// Returns a `serde_json::Value` — the caller serializes it to a string,
/// writes it to a file, or embeds it wherever needed.
pub fn generate_schema(schema: &crate::runtime::Schema) -> Value {
    generate_schema_from_ref(SchemaRef::from_dynamic(schema))
}

/// Internal entry point. Walks any `SchemaRef`-backed schema and emits the
/// JSON Schema document.
pub(crate) fn generate_schema_from_ref(schema: SchemaRef<'_>) -> Value {
    let mut root = schema_to_object(schema);
    if let Value::Object(map) = &mut root {
        map.insert("$schema".into(), Value::String(SCHEMA_DIALECT.into()));
    }
    root
}

/// Convert a schema node into a JSON Schema object.
fn schema_to_object(schema: SchemaRef<'_>) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    obj.insert("title".into(), Value::String(schema.name().into()));
    let schema_doc = schema.doc();
    if !schema_doc.is_empty() {
        obj.insert("description".into(), Value::String(join_doc(schema_doc)));
    }

    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in schema.fields() {
        let (name, prop, is_required) = field_to_property(field);
        if is_required {
            required.push(Value::String(name.clone()));
        }
        properties.insert(name, prop);
    }

    obj.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        obj.insert("required".into(), Value::Array(required));
    }
    obj.insert("patternProperties".into(), comment_key_allowlist());
    obj.insert("additionalProperties".into(), Value::Bool(false));

    Value::Object(obj)
}

/// Convert a [`FieldRef`] into a `(name, schema, required)` triple.
///
/// `required` is `true` for non-optional leaves and for all nested structs
/// (a nested struct has its own internal required list).
fn field_to_property(field: FieldRef<'_>) -> (String, Value, bool) {
    match field.kind {
        FieldKindRef::Nested { schema: nested } => {
            let mut schema = schema_to_object(nested);
            if !field.doc.is_empty()
                && let Value::Object(map) = &mut schema
            {
                map.insert("description".into(), Value::String(join_doc(field.doc)));
            }
            (field.name.into(), schema, true)
        }
        FieldKindRef::ArrayOf { schema: item } => {
            // JSON Schema for a TOML `[[name]]` array of items: `type: array`
            // with `items: <item schema>`. Each runtime array entry is itself
            // typed against `item`, so the per-item schema is the natural
            // place to declare structure.
            //
            // Not marked required: `DynamicSpec::finalize` treats an absent
            // array-of as the empty list (no entries), so a JSON Schema
            // requiring the property would reject configs clapfig accepts.
            let mut prop = Map::new();
            if !field.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(field.doc)));
            }
            prop.insert("type".into(), Value::String("array".into()));
            prop.insert("items".into(), schema_to_object(item));
            (field.name.into(), Value::Object(prop), false)
        }
        FieldKindRef::MapOf { schema: item } => {
            // TOML `[name.<key>]` with arbitrary entry keys. JSON Schema
            // models this as `type: object` with `additionalProperties:
            // <entry schema>` — entry keys are user-supplied so there are
            // no fixed properties, but each value must satisfy the item
            // schema.
            //
            // Not marked required: `DynamicSpec::finalize` treats an
            // absent map-of as the empty map (no entries).
            let mut prop = Map::new();
            if !field.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(field.doc)));
            }
            prop.insert("type".into(), Value::String("object".into()));
            // Comment keys inside a map-of instance are comments, not
            // entries — allowlist them so they escape the entry schema.
            prop.insert("patternProperties".into(), comment_key_allowlist());
            prop.insert("additionalProperties".into(), schema_to_object(item));
            (field.name.into(), Value::Object(prop), false)
        }
        FieldKindRef::Leaf(leaf) => {
            let mut prop = Map::new();
            if !field.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(field.doc)));
            }
            populate_leaf(&mut prop, leaf);
            (field.name.into(), Value::Object(prop), !leaf.optional)
        }
    }
}

/// Apply a leaf's declared type, default, env hint, and allowed-value set
/// onto its JSON Schema object.
fn populate_leaf(prop: &mut Map<String, Value>, leaf: LeafRef<'_>) {
    if let Some(name) = leaf_type_json_name(leaf.ty) {
        prop.insert("type".into(), Value::String(name.into()));
        if let LeafType::Array(item) = leaf.ty
            && let Some(item_name) = leaf_type_json_name(item)
        {
            let mut items = Map::new();
            items.insert("type".into(), Value::String(item_name.into()));
            prop.insert("items".into(), Value::Object(items));
        }
    }

    if let Some(default) = leaf.default
        && let Some(default_value) = value_to_json(default)
    {
        prop.insert("default".into(), default_value);
    }

    if let Some(env_name) = leaf.env {
        prop.insert("x-env".into(), Value::String(env_name.into()));
    }

    if let Some(values) = leaf.allowed_values() {
        let enum_array: Vec<Value> = values.iter().filter_map(value_to_json).collect();
        if !enum_array.is_empty() {
            prop.insert("enum".into(), Value::Array(enum_array));
        }
    }
}

/// JSON Schema `type` name for a runtime [`LeafType`]. `Enum` returns the
/// underlying primitive type implied by the first allowed value (callers
/// also emit `enum: [...]` separately).
fn leaf_type_json_name(ty: &LeafType) -> Option<&'static str> {
    match ty {
        LeafType::String => Some("string"),
        LeafType::Integer => Some("integer"),
        LeafType::Float => Some("number"),
        LeafType::Bool => Some("boolean"),
        LeafType::DateTime => Some("string"),
        LeafType::Array(_) => Some("array"),
        LeafType::Map(_) => Some("object"),
        LeafType::Enum { values } => values.first().and_then(value_json_type),
        // Unconstrained: JSON Schema convention is to omit `type` entirely,
        // signalling that any value is acceptable. Callers reading the
        // schema are expected to validate the value themselves.
        LeafType::Value => None,
    }
}

/// Map an owned config [`ConfigValue`] to its JSON Schema `type` name.
fn value_json_type(value: &ConfigValue) -> Option<&'static str> {
    match value {
        ConfigValue::String(_) => Some("string"),
        ConfigValue::Integer(_) => Some("integer"),
        ConfigValue::Float(_) => Some("number"),
        ConfigValue::Boolean(_) => Some("boolean"),
        _ => None,
    }
}

/// Convert an owned config [`ConfigValue`] into a JSON value for the
/// `default` and `enum` slots.
///
/// Unrepresentable values (`Datetime`, non-finite floats — JSON has no
/// literal for them) are dropped rather than emitted as a misleading
/// `null`. Arrays and maps convert recursively; entries that can't be
/// represented are skipped.
fn value_to_json(value: &ConfigValue) -> Option<Value> {
    match value {
        ConfigValue::String(s) => Some(Value::String(s.clone())),
        ConfigValue::Integer(i) => Some(json!(i)),
        ConfigValue::Float(f) => f.is_finite().then(|| json!(f)),
        ConfigValue::Boolean(b) => Some(Value::Bool(*b)),
        ConfigValue::Array(items) => Some(Value::Array(
            items.iter().filter_map(value_to_json).collect(),
        )),
        ConfigValue::Map(entries) => {
            let mut obj = Map::new();
            for (key, val) in entries {
                if let Some(v) = value_to_json(val) {
                    obj.insert(key.clone(), v);
                }
            }
            Some(Value::Object(obj))
        }
        ConfigValue::Datetime(_) => None,
    }
}

fn join_doc(source: &[String]) -> String {
    source
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;

    fn schema() -> Value {
        generate_schema(&test_schema())
    }

    #[test]
    fn non_finite_floats_are_skipped_not_null() {
        // JSON has no literal for NaN/±inf; the drop-not-null rule that
        // covers Datetime applies to them too (serde_json's `json!` would
        // otherwise silently emit `null`).
        assert_eq!(value_to_json(&ConfigValue::Float(f64::NAN)), None);
        assert_eq!(value_to_json(&ConfigValue::Float(f64::INFINITY)), None);
        assert_eq!(value_to_json(&ConfigValue::Float(f64::NEG_INFINITY)), None);
        assert_eq!(
            value_to_json(&ConfigValue::Float(1.5)),
            Some(json!(1.5)),
            "finite floats still convert"
        );
        // Recursive skip: the array entry vanishes rather than nulling.
        assert_eq!(
            value_to_json(&ConfigValue::Array(vec![
                ConfigValue::Float(f64::NAN),
                ConfigValue::Float(2.5),
            ])),
            Some(json!([2.5]))
        );
    }

    #[test]
    fn root_has_schema_dialect_and_type_object() {
        let s = schema();
        assert_eq!(s["$schema"], SCHEMA_DIALECT);
        assert_eq!(s["type"], "object");
        assert_eq!(s["title"], "TestConfig");
    }

    #[test]
    fn root_lists_top_level_properties() {
        let s = schema();
        let props = s["properties"].as_object().unwrap();
        assert!(props.contains_key("host"));
        assert!(props.contains_key("port"));
        assert!(props.contains_key("debug"));
        assert!(props.contains_key("database"));
    }

    #[test]
    fn types_emitted_from_declared_leaf_types() {
        let s = schema();
        let props = &s["properties"];
        assert_eq!(props["host"]["type"], "string");
        assert_eq!(props["port"]["type"], "integer");
        assert_eq!(props["debug"]["type"], "boolean");
    }

    #[test]
    fn defaults_emitted_on_properties() {
        let s = schema();
        let props = &s["properties"];
        assert_eq!(props["host"]["default"], "localhost");
        assert_eq!(props["port"]["default"], 8080);
        assert_eq!(props["debug"]["default"], false);
    }

    #[test]
    fn doc_comments_become_descriptions() {
        let s = schema();
        let props = &s["properties"];
        assert!(
            props["host"]["description"]
                .as_str()
                .unwrap()
                .contains("host")
        );
        assert!(
            props["port"]["description"]
                .as_str()
                .unwrap()
                .contains("port")
        );
    }

    #[test]
    fn nested_struct_becomes_object_with_own_properties() {
        let s = schema();
        let db = &s["properties"]["database"];
        assert_eq!(db["type"], "object");
        assert_eq!(db["title"], "TestDbConfig");
        let db_props = db["properties"].as_object().unwrap();
        assert!(db_props.contains_key("url"));
        assert!(db_props.contains_key("pool_size"));
        assert_eq!(db_props["pool_size"]["type"], "integer");
        assert_eq!(db_props["pool_size"]["default"], 5);
    }

    #[test]
    fn required_array_excludes_optional_fields() {
        let s = schema();
        let root_required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(root_required.contains(&"host"));
        assert!(root_required.contains(&"port"));
        assert!(root_required.contains(&"debug"));
        assert!(root_required.contains(&"database"));

        let db_required: Vec<&str> = s["properties"]["database"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(db_required.contains(&"pool_size"));
        // url is optional — must NOT be required.
        assert!(!db_required.contains(&"url"));
    }

    #[test]
    fn optional_field_still_appears_in_properties() {
        let s = schema();
        let db_props = s["properties"]["database"]["properties"]
            .as_object()
            .unwrap();
        assert!(db_props.contains_key("url"));
        // Declared string type is emitted even without a default; there is
        // no `default` key.
        assert_eq!(db_props["url"]["type"], "string");
        assert!(!db_props["url"].as_object().unwrap().contains_key("default"));
    }

    #[test]
    fn additional_properties_false_on_objects() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["database"]["additionalProperties"], false);
    }

    #[test]
    fn comment_key_pattern_allowlisted_on_every_object() {
        // ADR-0002: `additionalProperties: false` plus the `^//`
        // patternProperties allowlist, so third-party validators accept
        // documented JSON templates. The empty schema accepts any comment
        // value shape (string or array-of-strings prose).
        let s = schema();
        assert_eq!(s["patternProperties"]["^//"], json!({}));
        assert_eq!(
            s["properties"]["database"]["patternProperties"]["^//"],
            json!({})
        );
    }

    #[test]
    fn map_of_object_allowlists_comment_keys_beside_entry_schema() {
        use crate::runtime::{Field, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .map_of(
                    "servers",
                    RtSchema::object("Server").field("host", Field::string().default("x")),
                )
                .build(),
        );
        let servers = &s["properties"]["servers"];
        // Entries validate against the item schema; comment keys escape it.
        assert_eq!(servers["patternProperties"]["^//"], json!({}));
        assert_eq!(servers["additionalProperties"]["type"], "object");
    }

    #[test]
    fn enum_leaf_emits_enum_array_and_primitive_type() {
        let s = generate_schema(&crate::fixtures::test::enum_schema());
        let mode = &s["properties"]["mode"];
        assert_eq!(mode["type"], "string");
        let allowed: Vec<&str> = mode["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(allowed, vec!["fast", "slow"]);
    }

    #[test]
    fn optional_field_has_no_null_default_key() {
        // Regression guard: the emitter must not fabricate a `default: null`
        // for fields that have no default.
        let s = schema();
        let url = &s["properties"]["database"]["properties"]["url"];
        let url_obj = url.as_object().unwrap();
        assert!(
            !url_obj.contains_key("default"),
            "optional field must not have a default key: {url}"
        );
    }

    #[test]
    fn schema_serializes_to_valid_json() {
        let s = schema();
        let json_text = serde_json::to_string_pretty(&s).unwrap();
        let reparsed: Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(reparsed, s);
    }
}
