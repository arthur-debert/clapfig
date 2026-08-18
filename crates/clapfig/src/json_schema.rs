//! JSON Schema generation from a config schema.
//!
//! Entry point is [`generate_schema`]: it walks a
//! [`runtime::Schema`](crate::runtime::Schema) — built by the runtime
//! builder or emitted by `#[derive(clapfig::Schema)]` — and produces a JSON
//! Schema document. Useful for auto-generating UI editors, external
//! validation tools, or IDE integrations.
//!
//! The walker consumes `&runtime::Schema` directly — the crate's one
//! schema model — so the same generator serves both entry points.
//!
//! # What is in the schema
//!
//! - **Structure**: every nested config object becomes a JSON `object` with
//!   `properties`.
//! - **Required**: `required` mirrors what the runtime actually rejects
//!   when absent — a leaf is required only if it is non-optional AND has
//!   no default (defaults are synthesized during finalization), and a
//!   nested section is required only if it transitively contains such a
//!   leaf. An external validator therefore accepts exactly the documents
//!   clapfig loads.
//! - **Docs**: schema and field doc lines become `description`.
//! - **Types**: converted recursively from each leaf's declared
//!   [`LeafType`] — including leaves without defaults. String →
//!   `"string"`, integer → `"integer"` (with declared bounds as
//!   `minimum`/`maximum`), float → `"number"`, bool → `"boolean"`,
//!   datetime → `"string"` with an `anyOf` covering TOML's four lexical
//!   forms (range-aware patterns; `format: "date-time"` / `"date"` only
//!   on the branches those formats actually describe), array → `"array"`
//!   with a recursive
//!   `items` schema, map → `"object"` with a recursive
//!   `additionalProperties` value schema.
//! - **Defaults**: the literal default value (when present) is emitted as
//!   `default` on the property (datetimes in their lexical string form).
//! - **Enums**: `Enum { values }` leaves emit `enum: [...]`. A single
//!   `type` is added only when every allowed value shares one JSON
//!   primitive type — a mixed set (`"auto"` and `0`) is constrained by
//!   `enum` alone, so an external validator still accepts every value
//!   clapfig does. Applies at any nesting depth (an `Array(Enum)` leaf
//!   constrains its `items`).
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
//! use clapfig::json_schema;
//!
//! let value = json_schema::generate_schema(MyConfig::schema());
//! println!("{}", serde_json::to_string_pretty(&value).unwrap());
//! ```

use serde_json::{Map, Value, json};

use crate::runtime::{Field, Leaf, LeafType, NamedField, Schema};
use crate::value::Value as ConfigValue;

/// JSON Schema dialect emitted in the root `$schema` field.
const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The reserved JSON comment-key namespace (ADR-0002), allowlisted on
/// every object via `patternProperties` so documented JSON templates
/// validate against the schema in third-party tooling. Keys matching the
/// pattern escape `additionalProperties: false` (and, on map-of objects,
/// the entry schema).
const COMMENT_KEY_PATTERN: &str = "^//";

/// RFC 3339 offset date-time (`T` + `Z`/`±hh:mm`) — the form
/// JSON Schema `format: "date-time"` describes. Month 01–12, day
/// 01–31, hour 00–23, minute 00–59, second 00–60 (leap second),
/// offset hour 00–23 / minute 00–59. Leap-year and month-length
/// rules stay with the runtime parser.
const DATETIME_RFC3339_PATTERN: &str = concat!(
    r"^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])",
    r"T",
    r"([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?",
    r"(Z|[+-]([01][0-9]|2[0-3]):[0-5][0-9])",
    r"$",
);

/// TOML offset date-time, including `T`/`t`/space and `Z`/`z`.
const DATETIME_OFFSET_PATTERN: &str = concat!(
    r"^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])",
    r"[Tt ]",
    r"([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?",
    r"(Z|z|[+-]([01][0-9]|2[0-3]):[0-5][0-9])",
    r"$",
);

/// TOML local date-time (`1979-05-27T07:32:00`) — no offset.
const DATETIME_LOCAL_DATETIME_PATTERN: &str = concat!(
    r"^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])",
    r"[Tt ]",
    r"([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?",
    r"$",
);

/// TOML local date (`1979-05-27`).
const DATETIME_LOCAL_DATE_PATTERN: &str =
    concat!(r"^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])", r"$",);

/// TOML local time (`07:32:00`, optional fraction).
const DATETIME_LOCAL_TIME_PATTERN: &str = concat!(
    r"^",
    r"([01][0-9]|2[0-3]):[0-5][0-9]:([0-5][0-9]|60)(\.[0-9]+)?",
    r"$",
);

/// The `patternProperties` object allowlisting [`COMMENT_KEY_PATTERN`]
/// (the empty schema `{}` accepts any comment value shape).
fn comment_key_allowlist() -> Value {
    json!({ COMMENT_KEY_PATTERN: {} })
}

/// Generate a JSON Schema document from a config schema.
///
/// Works for both entry points: pass the schema handed to
/// [`Clapfig::builder`](crate::Clapfig::builder), or a derive-emitted
/// schema via `C::schema()`.
///
/// Returns a `serde_json::Value` — the caller serializes it to a string,
/// writes it to a file, or embeds it wherever needed.
pub fn generate_schema(schema: &Schema) -> Value {
    let mut root = schema_to_object(schema);
    if let Value::Object(map) = &mut root {
        map.insert("$schema".into(), Value::String(SCHEMA_DIALECT.into()));
    }
    root
}

/// Convert a schema node into a JSON Schema object.
fn schema_to_object(schema: &Schema) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    obj.insert("title".into(), Value::String(schema.name.clone()));
    if !schema.doc.is_empty() {
        obj.insert("description".into(), Value::String(join_doc(&schema.doc)));
    }

    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in &schema.fields {
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

/// Convert a [`NamedField`] into a `(name, schema, required)` triple.
///
/// `required` mirrors the runtime's absence rules: `true` for a leaf that
/// is non-optional AND defaultless AND not map-typed (an absent
/// non-optional map leaf materializes as the empty map, like structural
/// `MapOf`/`ArrayOf` nodes), and for a nested struct that transitively
/// contains such a leaf ([`schema_requires_presence`]).
fn field_to_property(field: &NamedField) -> (String, Value, bool) {
    match &field.field {
        Field::Nested(nested) => {
            let schema = schema_to_object(nested);
            (field.name.clone(), schema, schema_requires_presence(nested))
        }
        Field::ArrayOf(item) => {
            // JSON Schema for a TOML `[[name]]` array of items: `type: array`
            // with `items: <item schema>`. Each runtime array entry is itself
            // typed against `item`, so the per-item schema is the natural
            // place to declare structure.
            //
            // Not marked required: finalization treats an absent
            // array-of as the empty list (no entries), so a JSON Schema
            // requiring the property would reject configs clapfig accepts.
            let mut prop = Map::new();
            if !item.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&item.doc)));
            }
            prop.insert("type".into(), Value::String("array".into()));
            prop.insert("items".into(), schema_to_object(item));
            (field.name.clone(), Value::Object(prop), false)
        }
        Field::MapOf(item) => {
            // TOML `[name.<key>]` with arbitrary entry keys. JSON Schema
            // models this as `type: object` with `additionalProperties:
            // <entry schema>` — entry keys are user-supplied so there are
            // no fixed properties, but each value must satisfy the item
            // schema.
            //
            // Not marked required: finalization treats an
            // absent map-of as the empty map (no entries).
            let mut prop = Map::new();
            if !item.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&item.doc)));
            }
            prop.insert("type".into(), Value::String("object".into()));
            // Comment keys inside a map-of instance are comments, not
            // entries — allowlist them so they escape the entry schema.
            prop.insert("patternProperties".into(), comment_key_allowlist());
            prop.insert("additionalProperties".into(), schema_to_object(item));
            (field.name.clone(), Value::Object(prop), false)
        }
        Field::Leaf(leaf) => {
            let mut prop = Map::new();
            if !leaf.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&leaf.doc)));
            }
            populate_leaf(&mut prop, leaf);
            // Required only when the runtime rejects the absence:
            // non-optional AND defaultless AND not map-typed — an absent
            // non-optional map leaf materializes as the empty map.
            let required =
                !leaf.optional && leaf.default.is_none() && !matches!(leaf.ty, LeafType::Map(_));
            (field.name.clone(), Value::Object(prop), required)
        }
    }
}

/// `true` when the runtime rejects a document that omits this schema
/// subtree entirely: it transitively contains a non-optional leaf with no
/// default. Finalization synthesizes defaults into absent sections, so a
/// section whose required leaves all carry defaults is satisfiable when
/// absent — exporting it `required` would make external validators reject
/// configs clapfig loads fine. `ArrayOf`/`MapOf` subtrees never require
/// presence (absent means the empty list/map), and neither do map-typed
/// leaves (an absent non-optional map leaf materializes as the empty map).
fn schema_requires_presence(schema: &Schema) -> bool {
    schema.fields.iter().any(|nf| match &nf.field {
        Field::Leaf(leaf) => {
            !leaf.optional && leaf.default.is_none() && !matches!(leaf.ty, LeafType::Map(_))
        }
        Field::Nested(nested) => schema_requires_presence(nested),
        Field::ArrayOf(_) | Field::MapOf(_) => false,
    })
}

/// Apply a leaf's declared type, default, and env hint onto its JSON
/// Schema object.
fn populate_leaf(prop: &mut Map<String, Value>, leaf: &Leaf) {
    if let Some(ty_schema) = leaf_type_to_schema(&leaf.ty) {
        for (key, value) in ty_schema {
            prop.insert(key, value);
        }
    }

    if let Some(default) = &leaf.default
        && let Some(default_value) = value_to_json(default)
    {
        prop.insert("default".into(), default_value);
    }

    if let Some(env_name) = &leaf.env {
        prop.insert("x-env".into(), Value::String(env_name.clone()));
    }
}

/// Recursively convert a runtime [`LeafType`] into the JSON Schema object
/// constraining values of that type.
///
/// Containers recurse for full fidelity: an array leaf constrains its
/// `items` (nested arrays keep their inner `items`, `Array(Enum)` keeps
/// the enum constraint), and a map leaf constrains entry values via
/// `additionalProperties` (with the ADR-0002 `^//` comment-key allowlist,
/// since comment keys inside a map instance are comments, not entries).
/// `Enum` emits `enum: [...]`. A single `type` is added only when every
/// allowed value shares one JSON primitive type; mixed sets (`"auto"`
/// and `0`) omit `type` so `enum` alone constrains the value.
///
/// Returns `None` for [`LeafType::Value`]: JSON Schema convention is to
/// omit the constraint entirely, signalling that any value is acceptable.
/// Callers reading the schema are expected to validate the value
/// themselves.
fn leaf_type_to_schema(ty: &LeafType) -> Option<Map<String, Value>> {
    let mut obj = Map::new();
    match ty {
        LeafType::String => {
            obj.insert("type".into(), Value::String("string".into()));
        }
        LeafType::Integer { min, max } => {
            obj.insert("type".into(), Value::String("integer".into()));
            if let Some(lo) = min {
                obj.insert("minimum".into(), json!(lo));
            }
            if let Some(hi) = max {
                obj.insert("maximum".into(), json!(hi));
            }
        }
        LeafType::Float => {
            obj.insert("type".into(), Value::String("number".into()));
        }
        LeafType::Bool => {
            obj.insert("type".into(), Value::String("boolean".into()));
        }
        LeafType::DateTime => {
            obj.extend(datetime_type_schema());
        }
        LeafType::Array(elem) => {
            obj.insert("type".into(), Value::String("array".into()));
            if let Some(items) = leaf_type_to_schema(elem) {
                obj.insert("items".into(), Value::Object(items));
            }
        }
        LeafType::Map(elem) => {
            obj.insert("type".into(), Value::String("object".into()));
            if let Some(entry) = leaf_type_to_schema(elem) {
                // Comment keys inside a map instance are comments, not
                // entries — allowlist them so they escape the value schema.
                obj.insert("patternProperties".into(), comment_key_allowlist());
                obj.insert("additionalProperties".into(), Value::Object(entry));
            }
        }
        LeafType::Enum { values } => {
            if let Some(name) = homogeneous_json_type(values) {
                obj.insert("type".into(), Value::String(name.into()));
            }
            let enum_array: Vec<Value> = values.iter().filter_map(value_to_json).collect();
            if !enum_array.is_empty() {
                obj.insert("enum".into(), Value::Array(enum_array));
            }
        }
        LeafType::Value => return None,
    }
    Some(obj)
}

/// JSON Schema for a datetime leaf: `type: string` plus an `anyOf`
/// covering TOML's four lexical forms.
///
/// `format: "date-time"` is only RFC 3339 with a required offset, so it
/// is never placed on the leaf itself (that would reject local date,
/// local time, local date-time, and TOML's space/`t`/`z` variants).
/// It rides the RFC 3339 offset branch, next to a range-aware pattern;
/// `format: "date"` rides the local-date branch the same way. Local
/// date-time, local time, and TOML's extra offset spellings have no
/// matching format and are pattern-only. Patterns constrain month,
/// day, hour, minute, second, and offset ranges so a digit-only
/// schema cannot accept `1979-99-99` / `25:61:61` / `+99:99`.
fn datetime_type_schema() -> Map<String, Value> {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("string".into()));
    obj.insert("anyOf".into(), datetime_any_of());
    obj
}

fn datetime_any_of() -> Value {
    json!([
        { "format": "date-time", "pattern": DATETIME_RFC3339_PATTERN },
        { "pattern": DATETIME_OFFSET_PATTERN },
        { "pattern": DATETIME_LOCAL_DATETIME_PATTERN },
        { "format": "date", "pattern": DATETIME_LOCAL_DATE_PATTERN },
        { "pattern": DATETIME_LOCAL_TIME_PATTERN },
    ])
}

/// The single JSON Schema `type` name shared by every value, or `None`
/// when the set is empty, mixed, or contains a non-primitive.
fn homogeneous_json_type(values: &[ConfigValue]) -> Option<&'static str> {
    let mut seen: Option<&'static str> = None;
    for value in values {
        let name = value_json_type(value)?;
        match seen {
            None => seen = Some(name),
            Some(prev) if prev != name => return None,
            Some(_) => {}
        }
    }
    seen
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
/// Datetimes emit as their lexical string form — matching the
/// `type: string` plus four-form `anyOf` their leaves declare.
/// Unrepresentable values (non-finite floats — JSON has no literal for
/// them) are dropped rather than emitted as a misleading `null`. Arrays
/// and maps convert recursively; entries that can't be represented are
/// skipped.
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
        ConfigValue::Datetime(d) => Some(Value::String(crate::value::lexical_string(d))),
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
        // JSON has no literal for NaN/±inf; drop them rather than let
        // serde_json's `json!` silently emit `null`.
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
    fn required_mirrors_runtime_absence_rules() {
        // The test schema's leaves are all defaulted or optional, and
        // finalization synthesizes defaults for absent fields/sections —
        // the runtime loads `{}` fine, so NOTHING may be `required` or an
        // external validator would reject documents clapfig accepts.
        let s = schema();
        assert!(
            s.get("required").is_none(),
            "all-defaulted root must have no required array: {s}"
        );
        assert!(
            s["properties"]["database"].get("required").is_none(),
            "all-satisfiable section must have no required array"
        );
    }

    #[test]
    fn required_lists_defaultless_leaves_and_sections_containing_them() {
        use crate::runtime::{Field, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .field("name", Field::string()) // required, no default
                .field("host", Field::string().default("localhost"))
                .nested(
                    "auth",
                    RtSchema::object("Auth").field("token", Field::string()),
                )
                .nested(
                    "limits",
                    RtSchema::object("Limits").field("max", Field::integer().default(10i64)),
                )
                .build(),
        );
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // The defaultless leaf and the section transitively containing one.
        assert!(required.contains(&"name"));
        assert!(required.contains(&"auth"));
        // Defaulted leaf and all-satisfiable section are absence-safe.
        assert!(!required.contains(&"host"));
        assert!(!required.contains(&"limits"));
        // Inside `auth`, the defaultless leaf is required.
        let auth_required: Vec<&str> = s["properties"]["auth"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(auth_required, vec!["token"]);
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
    fn container_leaf_types_convert_recursively() {
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .field(
                    "matrix",
                    Field::array_of_type(LeafType::Array(Box::new(LeafType::Integer {
                        min: None,
                        max: None,
                    })))
                    .optional(),
                )
                .field(
                    "modes",
                    Field::array_of_type(LeafType::Enum {
                        values: vec!["fast".into(), "slow".into()],
                    })
                    .optional(),
                )
                .field("weights", Field::map_of(LeafType::Float).optional())
                .field("extras", Field::map_of(LeafType::Value).optional())
                .build(),
        );
        let props = &s["properties"];
        // Nested array keeps its inner `items`.
        assert_eq!(props["matrix"]["items"]["type"], "array");
        assert_eq!(props["matrix"]["items"]["items"]["type"], "integer");
        // Array(Enum) keeps the enum constraint on items; a homogeneous
        // set still carries the shared primitive type.
        assert_eq!(props["modes"]["items"]["type"], "string");
        assert_eq!(props["modes"]["items"]["enum"], json!(["fast", "slow"]));
        // Map(elem) constrains entry values and allowlists comment keys.
        assert_eq!(props["weights"]["type"], "object");
        assert_eq!(props["weights"]["additionalProperties"]["type"], "number");
        assert_eq!(props["weights"]["patternProperties"]["^//"], json!({}));
        // Map(Value) accepts anything — no constraint emitted.
        assert!(
            props["extras"]
                .as_object()
                .unwrap()
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn integer_bounds_emit_minimum_and_maximum() {
        use crate::runtime::{Field, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .field("retries", Field::integer_in(Some(0), Some(255)).optional())
                .field("count", Field::integer().optional())
                .build(),
        );
        let retries = &s["properties"]["retries"];
        assert_eq!(retries["type"], "integer");
        assert_eq!(retries["minimum"], 0);
        assert_eq!(retries["maximum"], 255);
        // Unbounded integers emit no bounds keys.
        let count = s["properties"]["count"].as_object().unwrap();
        assert!(!count.contains_key("minimum"));
        assert!(!count.contains_key("maximum"));
    }

    #[test]
    fn datetime_leaves_model_all_four_toml_forms() {
        use crate::runtime::{Field, Schema as RtSchema};
        fn dt(s: &str) -> ConfigValue {
            ConfigValue::Datetime(s.parse().unwrap())
        }
        let s = generate_schema(
            &RtSchema::object("App")
                .field(
                    "offset",
                    Field::datetime().default(dt("1979-05-27T07:32:00Z")),
                )
                .field(
                    "local_dt",
                    Field::datetime().default(dt("1979-05-27T07:32:00")),
                )
                .field("date", Field::datetime().default(dt("1979-05-27")))
                .field("time", Field::datetime().default(dt("07:32:00")))
                .build(),
        );
        let expected_any_of = datetime_any_of();
        for (name, default) in [
            ("offset", "1979-05-27T07:32:00Z"),
            ("local_dt", "1979-05-27T07:32:00"),
            ("date", "1979-05-27"),
            ("time", "07:32:00"),
        ] {
            let leaf = &s["properties"][name];
            assert_eq!(leaf["type"], "string", "{name}");
            assert!(
                leaf.get("format").is_none(),
                "{name}: format on the leaf itself would reject other TOML forms"
            );
            assert_eq!(leaf["anyOf"], expected_any_of, "{name}");
            assert_eq!(leaf["default"], default, "{name}");
        }
        assert_eq!(s["properties"]["date"]["anyOf"][3]["format"], "date");
        assert_eq!(s["properties"]["offset"]["anyOf"][0]["format"], "date-time");
    }

    /// Whether `candidate` matches any `pattern` in the datetime `anyOf`.
    /// JSON Schema patterns are ECMA-262; these patterns use a regex
    /// subset both dialects share, so the Rust regex crate is a fair
    /// stand-in without adding the jsonschema crate (MSRV / dep tree).
    fn datetime_schema_matches(candidate: &str) -> bool {
        let any_of = datetime_any_of();
        any_of
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|branch| branch.get("pattern").and_then(Value::as_str))
            .any(|pattern| {
                regex::Regex::new(pattern)
                    .unwrap_or_else(|e| {
                        panic!("schema pattern must be valid regex: {e}: {pattern}")
                    })
                    .is_match(candidate)
            })
    }

    #[test]
    fn datetime_patterns_reject_out_of_range_components() {
        for bad in [
            "1979-99-99",
            "1979-00-01",
            "1979-13-01",
            "25:61:61",
            "24:00:00",
            "07:60:00",
            "07:32:61",
            "1979-05-27T07:32:00+99:99",
            "1979-05-27T07:32:00+25:00",
            "1979-05-27T07:32:00+07:60",
        ] {
            assert!(
                !datetime_schema_matches(bad),
                "{bad} is not a valid TOML datetime and must fail the schema"
            );
        }
        for good in [
            "1979-05-27T07:32:00Z",
            "1979-05-27T00:32:00-07:00",
            "1979-05-27T07:32:00",
            "1979-05-27",
            "07:32:00",
            "1979-05-27 07:32:00Z",
            "1979-05-27t07:32:00z",
            "23:59:60",
            "07:32:00.5",
        ] {
            assert!(
                datetime_schema_matches(good),
                "{good} is a valid TOML datetime and must pass the schema"
            );
        }
    }

    #[test]
    fn heterogeneous_enum_omits_inferred_type() {
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let mixed = vec!["auto".into(), 0i64.into()];
        let s = generate_schema(
            &RtSchema::object("App")
                .field("choice", Field::enum_of(mixed.clone()).optional())
                .field(
                    "modes",
                    Field::array_of_type(LeafType::Enum {
                        values: mixed.clone(),
                    })
                    .optional(),
                )
                .field(
                    "labels",
                    Field::map_of(LeafType::Enum { values: mixed }).optional(),
                )
                .build(),
        );
        let props = &s["properties"];
        // enum alone constrains; a single inferred type would reject
        // the other allowed primitive.
        let choice = props["choice"].as_object().unwrap();
        assert!(!choice.contains_key("type"), "{choice:?}");
        assert_eq!(choice["enum"], json!(["auto", 0]));
        let items = props["modes"]["items"].as_object().unwrap();
        assert!(!items.contains_key("type"), "{items:?}");
        assert_eq!(items["enum"], json!(["auto", 0]));
        let entries = props["labels"]["additionalProperties"].as_object().unwrap();
        assert!(!entries.contains_key("type"), "{entries:?}");
        assert_eq!(entries["enum"], json!(["auto", 0]));
    }

    #[test]
    fn schema_serializes_to_valid_json() {
        let s = schema();
        let json_text = serde_json::to_string_pretty(&s).unwrap();
        let reparsed: Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(reparsed, s);
    }
}
