//! JSON Schema generation from a confique config struct.
//!
//! Walks `C::META` (confique's compile-time metadata tree) and produces a
//! JSON Schema document describing the config. Useful for auto-generating
//! UI editors, external validation tools, or IDE integrations.
//!
//! # What is in the schema
//!
//! - **Structure**: every nested config struct becomes a JSON `object` with
//!   `properties`; non-`Option<T>` fields are listed in `required`.
//! - **Docs**: struct and field `///` doc comments become `description`.
//! - **Types**: inferred from each field's `#[config(default = ...)]`
//!   expression. String → `"string"`, integer → `"integer"`, float →
//!   `"number"`, bool → `"boolean"`, array → `"array"`, map → `"object"`.
//! - **Defaults**: the literal default value (when present) is emitted as
//!   `default` on the property.
//! - **Env vars**: when a field maps to an env var, the name is attached as
//!   the non-standard `x-env` extension.
//!
//! # Limitation: types without defaults
//!
//! Confique's `Meta` tree does not carry Rust type information directly — it
//! only records the default-value *expression*. A field without a default and
//! without an explicit type hint therefore gets no `type` key in the schema
//! (i.e. any JSON value is accepted). This is acceptable for UI generation:
//! a form generator will still see the field and its docs, and users supply
//! values anyway.
//!
//! # Example
//!
//! ```ignore
//! use clapfig::schema;
//!
//! let value = schema::generate_schema::<MyConfig>();
//! println!("{}", serde_json::to_string_pretty(&value).unwrap());
//! ```

use confique::Config;
use confique::meta::{Expr, Field, FieldKind, LeafKind, MapEntry, MapKey, Meta};
use serde_json::{Map, Value, json};

/// JSON Schema dialect emitted in the root `$schema` field.
const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Generate a JSON Schema document from a confique config type.
///
/// Returns a `serde_json::Value` — the caller serializes it to a string,
/// writes it to a file, or embeds it wherever needed.
pub fn generate_schema<C: Config>() -> Value {
    let mut root = meta_to_object(&C::META);
    if let Value::Object(map) = &mut root {
        map.insert("$schema".into(), Value::String(SCHEMA_DIALECT.into()));
    }
    root
}

/// Convert a `confique::meta::Meta` node into a JSON Schema object.
fn meta_to_object(meta: &Meta) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    obj.insert("title".into(), Value::String(meta.name.into()));
    if !meta.doc.is_empty() {
        obj.insert("description".into(), Value::String(join_doc(meta.doc)));
    }

    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in meta.fields {
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
    obj.insert("additionalProperties".into(), Value::Bool(false));

    Value::Object(obj)
}

/// Convert a `Field` into a `(name, schema, required)` triple.
///
/// `required` is `true` for non-`Option<T>` leaves and for all nested
/// structs (a nested struct has its own internal required list).
fn field_to_property(field: &Field) -> (String, Value, bool) {
    match &field.kind {
        FieldKind::Nested { meta } => {
            let mut schema = meta_to_object(meta);
            if !field.doc.is_empty()
                && let Value::Object(map) = &mut schema
            {
                map.insert("description".into(), Value::String(join_doc(field.doc)));
            }
            (field.name.into(), schema, true)
        }
        FieldKind::Leaf { env, kind } => {
            let mut prop = Map::new();
            if !field.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(field.doc)));
            }

            let default = match kind {
                LeafKind::Required { default } => default.as_ref(),
                LeafKind::Optional => None,
            };

            if let Some(expr) = default {
                if let Some(ty) = infer_type(expr) {
                    prop.insert("type".into(), Value::String(ty.into()));
                }
                if let Some(default_value) = expr_to_json(expr) {
                    prop.insert("default".into(), default_value);
                }
            }

            if let Some(env_name) = env {
                prop.insert("x-env".into(), Value::String((*env_name).into()));
            }

            let required = matches!(kind, LeafKind::Required { .. });
            (field.name.into(), Value::Object(prop), required)
        }
    }
}

/// Infer a JSON Schema `type` string from a confique default expression.
fn infer_type(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Str(_) => Some("string"),
        Expr::Integer(_) => Some("integer"),
        Expr::Float(_) => Some("number"),
        Expr::Bool(_) => Some("boolean"),
        Expr::Array(_) => Some("array"),
        Expr::Map(_) => Some("object"),
        _ => None,
    }
}

/// Convert a confique `Expr` (default value) into a JSON value.
///
/// Returns `None` for variants we can't faithfully represent (confique's
/// `Expr` is `#[non_exhaustive]`), so the caller can omit the `default` key
/// entirely rather than emitting a misleading `null`.
fn expr_to_json(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Str(s) => Some(Value::String((*s).into())),
        Expr::Integer(i) => Some(json!(i)),
        Expr::Float(f) => Some(json!(f)),
        Expr::Bool(b) => Some(Value::Bool(*b)),
        Expr::Array(items) => Some(Value::Array(
            items.iter().filter_map(expr_to_json).collect(),
        )),
        Expr::Map(entries) => {
            let mut obj = Map::new();
            for MapEntry { key, value } in *entries {
                let Some(key_str) = map_key_to_string(key) else {
                    continue;
                };
                let Some(val) = expr_to_json(value) else {
                    continue;
                };
                obj.insert(key_str, val);
            }
            Some(Value::Object(obj))
        }
        _ => None,
    }
}

/// Render a `MapKey` as a JSON object key. Returns `None` for variants we
/// can't faithfully represent, so the caller can skip the entry rather than
/// collapsing distinct keys onto an empty string.
fn map_key_to_string(key: &MapKey) -> Option<String> {
    match key {
        MapKey::Str(s) => Some((*s).into()),
        MapKey::Integer(i) => Some(i.to_string()),
        MapKey::Float(f) => Some(f.to_string()),
        MapKey::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn join_doc(lines: &[&str]) -> String {
    lines
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
    use crate::fixtures::test::TestConfig;

    fn schema() -> Value {
        generate_schema::<TestConfig>()
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
    fn types_inferred_from_defaults() {
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
        // url is Option<String> — must NOT be required.
        assert!(!db_required.contains(&"url"));
    }

    #[test]
    fn optional_field_still_appears_in_properties() {
        let s = schema();
        let db_props = s["properties"]["database"]["properties"]
            .as_object()
            .unwrap();
        assert!(db_props.contains_key("url"));
        // No default and Option<T>, so no `type` and no `default`.
        assert!(!db_props["url"].as_object().unwrap().contains_key("type"));
        assert!(!db_props["url"].as_object().unwrap().contains_key("default"));
    }

    #[test]
    fn additional_properties_false_on_objects() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["database"]["additionalProperties"], false);
    }

    #[test]
    fn optional_field_has_no_null_default_key() {
        // Regression guard: expr_to_json must not fabricate a `default: null`
        // for fields that have no default (Option<T> / unrepresentable Expr).
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
