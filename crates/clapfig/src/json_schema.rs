//! JSON Schema generation from a config schema.
//!
//! Entry point is [`generate_schema`] (object root) or
//! [`generate_from_shape`] (any document-root [`Shape`](crate::runtime::Shape)).
//! A root map is `type: object` plus `additionalProperties` of the item
//! at the document root. Useful for auto-generating UI editors, external
//! validation tools, or IDE integrations.
//!
//! # What is in the schema
//!
//! - **Structure**: every nested config object becomes a JSON `object` with
//!   `properties`.
//! - **Required**: `required` mirrors what the runtime actually rejects
//!   when absent — a leaf is required only if it is non-optional AND has
//!   no default. [`Shape::Array`] / [`Shape::Map`] are never required
//!   (defaults are synthesized during finalization; an absent array/map
//!   materializes as the empty one), and a nested section is required
//!   only if it transitively contains a required leaf. An external
//!   validator therefore accepts exactly the documents clapfig loads.
//! - **Docs**: schema and field doc lines become `description`.
//! - **Types**: converted recursively from each node's [`Shape`](crate::runtime::Shape)
//!   / [`LeafType`]. String → `"string"`, integer → `"integer"` (with
//!   declared bounds as `minimum`/`maximum`), float → `"number"`, bool →
//!   `"boolean"`, datetime → `"string"` with an `anyOf` covering TOML's
//!   four lexical forms (range-aware patterns; `format: "date-time"` /
//!   `"date"` only on the branches those formats actually describe).
//!   [`Shape::Array`] → `"array"` with a recursive `items` schema;
//!   [`Shape::Map`] → `"object"` with a recursive `additionalProperties`
//!   value schema.
//! - **Defaults**: the literal default value (when present) is emitted as
//!   `default` on the property (datetimes in their lexical string form).
//!   Unrepresentable values (non-finite floats) omit the whole
//!   annotation rather than dropping members from a collection.
//! - **Enums**: `Enum { values }` leaves emit `enum: [...]`. A single
//!   `type` is added only when every allowed value shares one JSON
//!   primitive type — a mixed set (`"auto"` and `0`) is constrained by
//!   `enum` alone, so an external validator still accepts every value
//!   clapfig does. Applies at any nesting depth (a [`Shape::Array`] of an
//!   enum leaf constrains its `items`). If any member cannot be represented the
//!   `enum` annotation is omitted entirely.
//! - **Env vars**: when a field maps to an env var, the name is attached as
//!   the non-standard `x-env` extension.
//! - **Tagged unions**: an internally tagged shape is JSON Schema `oneOf`.
//!   Each branch is that variant's object schema plus the tag as a required
//!   property whose schema is `{ "type": "string", "const": "<discriminator>" }`.
//!   OpenAPI's `discriminator` keyword is not used.
//! - **JSON comment keys**: every object allowlists the `^//` key pattern
//!   (`patternProperties`) alongside `additionalProperties: false`. This is
//!   for third-party validators only — editors validating a documented JSON
//!   template against this schema directly. Clapfig's own validation never
//!   sees the keys: the JSON adapter strips the reserved `//` namespace at
//!   parse time (ADR-0002). Tagged `oneOf` branches are closed objects and
//!   carry the same allowlist.
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

use crate::runtime::{Leaf, LeafType, NamedField, Schema, Shape, TaggedShape, TaggedVariant};
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
    generate_from_shape(&Shape::Object(schema.clone()))
}

/// Generate a JSON Schema document from a document-root [`Shape`].
///
/// A root [`Shape::Map`] is `type: object` with `additionalProperties` of
/// the item schema at the document root (no synthetic parent property).
/// A root [`Shape::Tagged`] is `oneOf` of variant objects, each with the
/// tag as a required `{ "const": "<discriminator>" }` property. Object
/// roots match [`generate_schema`].
pub fn generate_from_shape(shape: &Shape) -> Value {
    let mut root = match shape {
        Shape::Object(schema) => schema_to_object(schema),
        Shape::Map(map) => map_root_to_object(map),
        Shape::Tagged(tagged) => tagged_to_schema(tagged),
        Shape::Leaf(_) | Shape::Array(_) => panic!(
            "clapfig: a Leaf or Array is not a legal document root (legal roots: Object, Map, Tagged)"
        ),
    };
    if let Value::Object(obj) = &mut root {
        obj.insert("$schema".into(), Value::String(SCHEMA_DIALECT.into()));
    }
    root
}

/// JSON Schema for a root homogeneous map: `type: object` plus
/// `additionalProperties` of the item, at the document root.
fn map_root_to_object(map: &crate::runtime::MapShape) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    if !map.name.is_empty() {
        obj.insert("title".into(), Value::String(map.name.clone()));
    }
    if !map.doc.is_empty() {
        obj.insert("description".into(), Value::String(join_doc(&map.doc)));
    }
    obj.insert("patternProperties".into(), comment_key_allowlist());
    if let Some(entry) = shape_to_schema(&map.item) {
        obj.insert("additionalProperties".into(), entry);
    }
    Value::Object(obj)
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
/// `required` mirrors the runtime's absence rules: `true` for a
/// [`Shape::Leaf`] that is non-optional AND defaultless, and for a nested
/// object that transitively contains such a leaf
/// ([`schema_requires_presence`]). [`Shape::Array`] and [`Shape::Map`]
/// are never required — an absent non-optional array/map materializes as
/// the empty array/map.
fn field_to_property(field: &NamedField) -> (String, Value, bool) {
    match &field.field {
        Shape::Object(nested) => {
            let schema = schema_to_object(nested);
            (field.name.clone(), schema, schema_requires_presence(nested))
        }
        Shape::Array(array) => {
            // JSON Schema for an array field: `type: array` with
            // `items: <item schema>`. Object items are TOML `[[name]]`;
            // leaf items are homogeneous arrays of leaves.
            //
            // Not marked required: finalization treats an absent array
            // as the empty list, so a JSON Schema requiring the property
            // would reject configs clapfig accepts.
            let mut prop = Map::new();
            if !array.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&array.doc)));
            }
            prop.insert("type".into(), Value::String("array".into()));
            if let Some(items) = shape_to_schema(&array.item) {
                prop.insert("items".into(), items);
            }
            populate_container_attrs(&mut prop, array.default.as_ref(), array.env.as_deref());
            (field.name.clone(), Value::Object(prop), false)
        }
        Shape::Map(map) => {
            // TOML `[name.<key>]` / homogeneous map: `type: object` with
            // `additionalProperties: <entry schema>`.
            let mut prop = Map::new();
            if !map.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&map.doc)));
            }
            prop.insert("type".into(), Value::String("object".into()));
            // Comment keys inside a map instance are comments, not
            // entries — allowlist them so they escape the entry schema.
            prop.insert("patternProperties".into(), comment_key_allowlist());
            if let Some(entry) = shape_to_schema(&map.item) {
                prop.insert("additionalProperties".into(), entry);
            }
            populate_container_attrs(&mut prop, map.default.as_ref(), map.env.as_deref());
            (field.name.clone(), Value::Object(prop), false)
        }
        Shape::Leaf(leaf) => {
            let mut prop = Map::new();
            if !leaf.doc.is_empty() {
                prop.insert("description".into(), Value::String(join_doc(&leaf.doc)));
            }
            populate_leaf(&mut prop, leaf);
            let required = !leaf.optional && leaf.default.is_none();
            (field.name.clone(), Value::Object(prop), required)
        }
        Shape::Tagged(tagged) => {
            // Absent tagged object → empty table → MissingRequired on the
            // tag, so the property is required on the parent.
            (field.name.clone(), tagged_to_schema(tagged), true)
        }
    }
}

fn shape_to_schema(shape: &Shape) -> Option<Value> {
    match shape {
        Shape::Object(schema) => Some(schema_to_object(schema)),
        Shape::Leaf(leaf) => leaf_type_to_schema(&leaf.ty).map(Value::Object),
        Shape::Array(array) => {
            let mut obj = Map::new();
            obj.insert("type".into(), Value::String("array".into()));
            if let Some(items) = shape_to_schema(&array.item) {
                obj.insert("items".into(), items);
            }
            Some(Value::Object(obj))
        }
        Shape::Map(map) => {
            let mut obj = Map::new();
            obj.insert("type".into(), Value::String("object".into()));
            obj.insert("patternProperties".into(), comment_key_allowlist());
            if let Some(entry) = shape_to_schema(&map.item) {
                obj.insert("additionalProperties".into(), entry);
            }
            Some(Value::Object(obj))
        }
        Shape::Tagged(tagged) => Some(tagged_to_schema(tagged)),
    }
}

/// JSON Schema for an internally tagged union: `oneOf` of variant objects,
/// each with the tag as a required `{ "const": "<discriminator>" }`
/// property. No OpenAPI `discriminator`.
fn tagged_to_schema(tagged: &TaggedShape) -> Value {
    let mut obj = Map::new();
    if !tagged.name.is_empty() {
        obj.insert("title".into(), Value::String(tagged.name.clone()));
    }
    if !tagged.doc.is_empty() {
        obj.insert("description".into(), Value::String(join_doc(&tagged.doc)));
    }
    let branches = tagged
        .variants
        .iter()
        .map(|variant| tagged_branch_schema(tagged, variant))
        .collect();
    obj.insert("oneOf".into(), Value::Array(branches));
    Value::Object(obj)
}

/// One `oneOf` branch: the variant object plus the tag field as a
/// required string `const`.
fn tagged_branch_schema(tagged: &TaggedShape, variant: &TaggedVariant) -> Value {
    let mut object = schema_to_object(&variant.schema);
    let Value::Object(map) = &mut object else {
        return object;
    };
    let tag_schema = json!({ "type": "string", "const": variant.discriminator });
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        let mut ordered = Map::new();
        ordered.insert(tagged.tag.clone(), tag_schema);
        for (key, value) in props.clone() {
            ordered.insert(key, value);
        }
        *props = ordered;
    }
    match map.get_mut("required") {
        Some(Value::Array(req)) => {
            if !req
                .iter()
                .any(|value| value.as_str() == Some(tagged.tag.as_str()))
            {
                req.insert(0, Value::String(tagged.tag.clone()));
            }
        }
        _ => {
            map.insert("required".into(), json!([tagged.tag]));
        }
    }
    object
}

fn populate_container_attrs(
    prop: &mut Map<String, Value>,
    default: Option<&crate::value::Value>,
    env: Option<&str>,
) {
    if let Some(default) = default
        && let Some(default_value) = value_to_json(default)
    {
        prop.insert("default".into(), default_value);
    }
    if let Some(env_name) = env {
        prop.insert("x-env".into(), Value::String(env_name.to_string()));
    }
}

/// `true` when the runtime rejects a document that omits this schema
/// subtree entirely: it transitively contains a non-optional leaf with no
/// default. Finalization synthesizes defaults into absent sections, so a
/// section whose required leaves all carry defaults is satisfiable when
/// absent — exporting it `required` would make external validators reject
/// configs clapfig loads fine. [`Shape::Array`] / [`Shape::Map`] subtrees
/// never require presence (absent means the empty list/map).
fn schema_requires_presence(schema: &Schema) -> bool {
    schema.fields.iter().any(|nf| match &nf.field {
        Shape::Leaf(leaf) => !leaf.optional && leaf.default.is_none(),
        Shape::Object(nested) => schema_requires_presence(nested),
        Shape::Array(_) | Shape::Map(_) => false,
        // Absent tagged object materializes as the empty table, then
        // MissingRequired on the tag — the parent must require this field.
        Shape::Tagged(_) => true,
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
        LeafType::Enum { values } => {
            if let Some(name) = homogeneous_json_type(values) {
                obj.insert("type".into(), Value::String(name.into()));
            }
            // All-or-nothing: a partial enum set would let external
            // validators accept (or reject) a different contract than
            // clapfig's runtime. Omit the annotation when any member
            // cannot be represented.
            if let Some(enum_array) = values.iter().map(value_to_json).collect::<Option<Vec<_>>>()
                && !enum_array.is_empty()
            {
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
/// them) yield `None` rather than a misleading `null`. Arrays and maps
/// convert recursively and all-or-nothing: if any member cannot be
/// represented the whole collection fails, so callers omit the complete
/// `default` / `enum` annotation instead of exporting a narrowed set.
fn value_to_json(value: &ConfigValue) -> Option<Value> {
    match value {
        ConfigValue::String(s) => Some(Value::String(s.clone())),
        ConfigValue::Integer(i) => Some(json!(i)),
        ConfigValue::Float(f) => f.is_finite().then(|| json!(f)),
        ConfigValue::Boolean(b) => Some(Value::Bool(*b)),
        ConfigValue::Array(items) => items
            .iter()
            .map(value_to_json)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        ConfigValue::Map(entries) => {
            let mut obj = Map::new();
            for (key, val) in entries {
                obj.insert(key.clone(), value_to_json(val)?);
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
        // All-or-nothing: a collection containing an unrepresentable
        // member fails as a whole rather than exporting a narrowed set.
        assert_eq!(
            value_to_json(&ConfigValue::Array(vec![
                ConfigValue::Float(f64::NAN),
                ConfigValue::Float(2.5),
            ])),
            None
        );
        assert_eq!(
            value_to_json(&ConfigValue::Map(
                std::iter::once(("a".into(), ConfigValue::Float(f64::NAN))).collect(),
            )),
            None
        );
        assert_eq!(
            value_to_json(&ConfigValue::Array(vec![ConfigValue::Float(2.5)])),
            Some(json!([2.5])),
            "all-representable collections still convert"
        );
    }

    #[test]
    fn unrepresentable_collection_omits_the_whole_annotation() {
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .field(
                    "samples",
                    Field::array_of_type(LeafType::Float)
                        .default(ConfigValue::Array(vec![
                            ConfigValue::Float(f64::NAN),
                            ConfigValue::Float(2.5),
                        ]))
                        .optional(),
                )
                .field(
                    "mode",
                    Field::enum_of([ConfigValue::Float(f64::NAN), ConfigValue::Float(1.0)])
                        .optional(),
                )
                .build(),
        );
        let samples = s["properties"]["samples"].as_object().unwrap();
        assert!(
            !samples.contains_key("default"),
            "partial default would lie to validators: {samples:?}"
        );
        let mode = s["properties"]["mode"].as_object().unwrap();
        assert!(
            !mode.contains_key("enum"),
            "narrowed enum would lie to validators: {mode:?}"
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
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let s = generate_schema(
            &RtSchema::object("App")
                .field("name", Field::string()) // required, no default
                .field("host", Field::string().default("localhost"))
                .field("tags", Field::array_of_type(LeafType::String))
                .field("labels", Field::map_of(LeafType::String))
                .nested(
                    "auth",
                    RtSchema::object("Auth").field("token", Field::string()),
                )
                .nested(
                    "limits",
                    RtSchema::object("Limits").field("max", Field::integer().default(10i64)),
                )
                .nested(
                    "meta",
                    RtSchema::object("Meta").field("tags", Field::array_of_type(LeafType::String)),
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
        // Defaulted leaf, all-satisfiable section, and array/map leaves
        // (absence materializes as []/{}) are absence-safe.
        assert!(!required.contains(&"host"));
        assert!(!required.contains(&"limits"));
        assert!(!required.contains(&"tags"));
        assert!(!required.contains(&"labels"));
        assert!(!required.contains(&"meta"));
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
                    Field::array_of_type(Field::array_of_type(Field::integer())).optional(),
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

    fn tagged_block() -> crate::runtime::TaggedShape {
        use crate::runtime::Schema as RtSchema;
        Shape::tagged("Block", "kind")
            .doc("A block instance.")
            .variant(
                "rust",
                RtSchema::object("Rust")
                    .field("mount", crate::runtime::Field::string())
                    .field("crate_path", crate::runtime::Field::string().optional())
                    .build(),
            )
            .variant(
                "payload",
                RtSchema::object("Payload")
                    .field("mount", crate::runtime::Field::string())
                    .field("artifact", crate::runtime::Field::string())
                    .build(),
            )
            .variant("off", RtSchema::object("Off").build())
            .build()
    }

    #[test]
    fn tagged_root_is_oneof_with_per_branch_tag_const() {
        let s = generate_from_shape(&Shape::Tagged(tagged_block()));
        assert_eq!(s["$schema"], SCHEMA_DIALECT);
        assert_eq!(s["title"], "Block");
        assert!(s["description"].as_str().unwrap().contains("block"), "{s}");
        assert!(
            s.get("discriminator").is_none(),
            "OpenAPI discriminator is not JSON Schema: {s}"
        );
        assert!(
            s.get("type").is_none(),
            "tagged root is oneOf, not type=object: {s}"
        );
        let one_of = s["oneOf"].as_array().expect("oneOf");
        assert_eq!(one_of.len(), 3);
        let rust = &one_of[0];
        assert_eq!(rust["type"], "object");
        assert_eq!(rust["properties"]["kind"]["const"], "rust");
        assert_eq!(rust["properties"]["kind"]["type"], "string");
        assert_eq!(rust["properties"]["mount"]["type"], "string");
        assert!(rust["properties"].get("artifact").is_none(), "{rust}");
        let required: Vec<&str> = rust["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required.contains(&"kind"), "{required:?}");
        assert!(required.contains(&"mount"), "{required:?}");
        assert!(!required.contains(&"crate_path"), "{required:?}");
        assert_eq!(rust["additionalProperties"], false);
        assert_eq!(rust["patternProperties"]["^//"], json!({}));
        assert_eq!(one_of[1]["properties"]["kind"]["const"], "payload");
        assert_eq!(one_of[2]["properties"]["kind"]["const"], "off");
        assert_eq!(one_of[2]["required"], json!(["kind"]));
    }

    #[test]
    fn nested_tagged_field_is_oneof_and_required() {
        use crate::runtime::Schema as RtSchema;
        let s = generate_schema(
            &RtSchema::object("App")
                .field("block", Shape::from(tagged_block()))
                .build(),
        );
        assert_eq!(s["patternProperties"]["^//"], json!({}));
        assert_eq!(s["additionalProperties"], false);
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, ["block"]);
        let block = &s["properties"]["block"];
        assert!(block.get("discriminator").is_none(), "{block}");
        assert_eq!(block["oneOf"].as_array().unwrap().len(), 3);
        assert_eq!(block["oneOf"][0]["properties"]["kind"]["const"], "rust");
    }

    #[test]
    fn map_of_tagged_composes_additional_properties_oneof() {
        use crate::runtime::Field;
        use crate::runtime::Schema as RtSchema;
        let s = generate_schema(
            &RtSchema::object("App")
                .field("blocks", Field::map_of(Shape::from(tagged_block())))
                .build(),
        );
        let blocks = &s["properties"]["blocks"];
        assert_eq!(blocks["type"], "object");
        assert_eq!(blocks["patternProperties"]["^//"], json!({}));
        let entry = &blocks["additionalProperties"];
        assert!(entry.get("discriminator").is_none(), "{entry}");
        assert_eq!(entry["oneOf"][0]["properties"]["kind"]["const"], "rust");
        assert_eq!(entry["oneOf"][0]["patternProperties"]["^//"], json!({}));
    }

    #[test]
    fn schema_serializes_to_valid_json() {
        let s = schema();
        let json_text = serde_json::to_string_pretty(&s).unwrap();
        let reparsed: Value = serde_json::from_str(&json_text).unwrap();
        assert_eq!(reparsed, s);
    }
}
