//! JSON adapter acceptance tests (value-model epic, WS04): the
//! self-consistency loop (`config gen` → parse → default-strict
//! validation), the format-parity JSON slice (same logical config in JSON
//! and TOML resolves identically, errors identically, and exports the
//! identical JSON Schema), the schema-driven datetime coercion of JSON
//! strings, and the exported schema validating the generated template
//! (comment keys allowlisted per ADR-0002).

use clapfig::error::ClapfigError;
use clapfig::runtime::{Field, Schema};
use clapfig::value::Value;
use clapfig::{Clapfig, ConfigAction, InputType, SearchPath};
use tempfile::TempDir;

/// The shared logical schema for the parity slice. Every required field
/// carries a default so the generated template resolves green on its own.
fn parity_schema() -> Schema {
    Schema::object("App")
        .doc("Parity demo schema")
        .field("host", Field::string().doc("App host").default("localhost"))
        .field("port", Field::integer().doc("Port number").default(8080i64))
        .field("ratio", Field::float().default(0.5))
        .field("debug", Field::boolean().default(false))
        .field("level", Field::enum_of(["debug", "info"]).default("info"))
        .nested(
            "db",
            Schema::object("Db")
                .doc("Database settings")
                .field("url", Field::string().optional())
                .field("pool_size", Field::integer().default(5i64)),
        )
        .build()
}

fn builder(schema: Schema, dir: &TempDir, file_name: &str) -> clapfig::Builder {
    Clapfig::builder(schema)
        .app_name("parity-demo")
        .file_name(file_name)
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
}

// --- self-consistency: gen → parse → default-strict validation ------------

#[test]
fn generated_json_template_resolves_green_under_default_strict() {
    let dir = TempDir::new().unwrap();
    let template = builder(parity_schema(), &dir, "app.json")
        .handle_to_string(&ConfigAction::Gen { output: None })
        .unwrap();
    // The rendered template is real JSON with "//" comment keys.
    assert!(
        template.contains("\"//\": \"Parity demo schema\""),
        "{template}"
    );
    std::fs::write(dir.path().join("app.json"), &template).unwrap();

    // Default-strict load: comment keys were stripped at parse, so no
    // unknown-key error fires and every value is the schema default.
    let table = builder(parity_schema(), &dir, "app.json").load().unwrap();
    assert_eq!(table["host"], Value::String("localhost".into()));
    assert_eq!(table["port"], Value::Integer(8080));
    let db = table["db"].as_map().unwrap();
    assert_eq!(db["pool_size"], Value::Integer(5));
    assert!(db.get("url").is_none());
}

#[test]
fn generated_json_template_honors_normalize_keys_and_resolves_green() {
    // normalize_keys(true) must reach JSON structurally: real keys, "//"
    // comment keys, and the assignment snippet inside a defaultless
    // field's comment all render kebab-case — and the generated template
    // then loads green under the same normalizing builder.
    let schema = || {
        Schema::object("App")
            .field("api_key", Field::string().doc("Required API key."))
            .nested(
                "db_settings",
                Schema::object("Db").field("pool_size", Field::integer().default(5i64)),
            )
            .build()
    };
    let dir = TempDir::new().unwrap();
    let template = builder(schema(), &dir, "app.json")
        .normalize_keys(true)
        .handle_to_string(&ConfigAction::Gen { output: None })
        .unwrap();
    assert!(template.contains(r#""//api-key""#), "{template}");
    assert!(
        template.contains(r#"\"api-key\": \"\""#),
        "snippet:\n{template}"
    );
    assert!(template.contains(r#""db-settings""#), "{template}");
    assert!(template.contains(r#""pool-size": 5"#), "{template}");
    assert!(!template.contains("api_key"), "no snake leak:\n{template}");
    assert!(
        !template.contains("pool_size"),
        "no snake leak:\n{template}"
    );

    // The kebab template resolves under the normalizing builder once the
    // defaultless field is supplied (paste the snippet, uncommented).
    let mut json: serde_json::Value = serde_json::from_str(&template).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("api-key".into(), serde_json::Value::String("k".into()));
    std::fs::write(
        dir.path().join("app.json"),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();
    let table = builder(schema(), &dir, "app.json")
        .normalize_keys(true)
        .load()
        .unwrap();
    assert_eq!(table["api_key"], Value::String("k".into()));
    assert_eq!(
        table["db_settings"].as_map().unwrap()["pool_size"],
        Value::Integer(5)
    );
}

// --- the format-parity JSON slice -----------------------------------------

const PARITY_TOML: &str = r#"
host = "example.com"
port = 9090
ratio = 1.25
debug = true
level = "debug"

[db]
url = "pg://prod"
pool_size = 9
"#;

const PARITY_JSON: &str = r#"{
  "//": "same logical config, JSON spelling",
  "host": "example.com",
  "port": 9090,
  "ratio": 1.25,
  "debug": true,
  "level": "debug",
  "db": {
    "//url": "primary",
    "url": "pg://prod",
    "pool_size": 9
  }
}"#;

#[test]
fn same_logical_config_resolves_to_identical_tree() {
    let toml_dir = TempDir::new().unwrap();
    std::fs::write(toml_dir.path().join("app.toml"), PARITY_TOML).unwrap();
    let from_toml = builder(parity_schema(), &toml_dir, "app.toml")
        .load()
        .unwrap();

    let json_dir = TempDir::new().unwrap();
    std::fs::write(json_dir.path().join("app.json"), PARITY_JSON).unwrap();
    let from_json = builder(parity_schema(), &json_dir, "app.json")
        .load()
        .unwrap();

    assert_eq!(from_toml, from_json);
}

#[test]
fn same_mistake_produces_identical_validation_error() {
    let toml_dir = TempDir::new().unwrap();
    std::fs::write(toml_dir.path().join("app.toml"), "port = \"nope\"\n").unwrap();
    let toml_err = builder(parity_schema(), &toml_dir, "app.toml")
        .load()
        .unwrap_err();

    let json_dir = TempDir::new().unwrap();
    std::fs::write(json_dir.path().join("app.json"), "{\"port\": \"nope\"}\n").unwrap();
    let json_err = builder(parity_schema(), &json_dir, "app.json")
        .load()
        .unwrap_err();

    match (toml_err, json_err) {
        (
            ClapfigError::InvalidValue {
                key: tk,
                reason: tr,
                origin: to,
            },
            ClapfigError::InvalidValue {
                key: jk,
                reason: jr,
                origin: jo,
            },
        ) => {
            assert_eq!(tk, jk);
            assert_eq!(tr, jr);
            assert_eq!(to.input_type, jo.input_type);
            assert_eq!(to.input_type, Some(InputType::File));
            assert!(to.span.is_some(), "TOML value span");
            assert!(jo.span.is_some(), "JSON value span");
        }
        (t, j) => panic!("expected matching InvalidValue, got {t:?} vs {j:?}"),
    }
}

#[test]
fn exported_json_schema_is_format_independent() {
    let toml_dir = TempDir::new().unwrap();
    let via_toml = builder(parity_schema(), &toml_dir, "app.toml")
        .handle_to_string(&ConfigAction::Schema { output: None })
        .unwrap();
    let json_dir = TempDir::new().unwrap();
    let via_json = builder(parity_schema(), &json_dir, "app.json")
        .handle_to_string(&ConfigAction::Schema { output: None })
        .unwrap();
    assert_eq!(via_toml, via_json);
}

// --- datetime: strings coerced by the schema-driven pass (ADR-0001) -------

#[test]
fn json_datetime_strings_coerce_to_the_same_tree_as_toml_natives() {
    let schema = || {
        Schema::object("Stamps")
            .field("offset", Field::datetime().default("1970-01-01T00:00:00Z"))
            .field("local_dt", Field::datetime().optional())
            .field("local_date", Field::datetime().optional())
            .field("local_time", Field::datetime().optional())
            .build()
    };

    // TOML: native datetimes in all four lexical forms.
    let toml_dir = TempDir::new().unwrap();
    std::fs::write(
        toml_dir.path().join("app.toml"),
        "offset = 1979-05-27T07:32:00Z\nlocal_dt = 1979-05-27T07:32:00\nlocal_date = 1979-05-27\nlocal_time = 07:32:00\n",
    )
    .unwrap();
    let from_toml = builder(schema(), &toml_dir, "app.toml").load().unwrap();

    // JSON: the same four forms as strings.
    let json_dir = TempDir::new().unwrap();
    std::fs::write(
        json_dir.path().join("app.json"),
        r#"{"offset": "1979-05-27T07:32:00Z", "local_dt": "1979-05-27T07:32:00", "local_date": "1979-05-27", "local_time": "07:32:00"}"#,
    )
    .unwrap();
    let from_json = builder(schema(), &json_dir, "app.json").load().unwrap();

    assert_eq!(from_toml, from_json);
    assert!(
        matches!(from_json["offset"], Value::Datetime(_)),
        "schema-driven coercion lands the owned Datetime variant"
    );
}

// --- the exported schema validates the generated template -----------------

/// Minimal JSON Schema walk covering exactly the constraints the exported
/// schema emits for objects: `properties`, `required`,
/// `patternProperties` (the `^//` comment allowlist), an
/// `additionalProperties` that is either `false` or an entry schema,
/// `type`, and `enum`. A real third-party validator enforces a superset
/// of this; the walk locks the contract the ADR promises — a documented
/// template is accepted, comment keys included.
fn validate(instance: &serde_json::Value, schema: &serde_json::Value) -> Result<(), String> {
    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array())
        && !allowed.contains(instance)
    {
        return Err(format!("{instance} not in enum {allowed:?}"));
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        None => return Ok(()),
        Some("string") if instance.is_string() => return Ok(()),
        Some("integer") if instance.is_i64() => return Ok(()),
        Some("number") if instance.is_number() => return Ok(()),
        Some("boolean") if instance.is_boolean() => return Ok(()),
        Some("array") => {
            let Some(items) = instance.as_array() else {
                return Err(format!("expected array, got {instance}"));
            };
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    validate(item, item_schema)?;
                }
            }
            return Ok(());
        }
        Some("object") => {}
        Some(expected) => return Err(format!("expected {expected}, got {instance}")),
    }

    let Some(obj) = instance.as_object() else {
        return Err(format!("expected object, got {instance}"));
    };
    let empty = serde_json::Map::new();
    let properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .unwrap_or(&empty);
    let comment_allowlisted = schema
        .get("patternProperties")
        .and_then(|p| p.get("^//"))
        .is_some();
    for (key, value) in obj {
        if let Some(prop_schema) = properties.get(key) {
            validate(value, prop_schema).map_err(|e| format!("{key}: {e}"))?;
        } else if key.starts_with("//") && comment_allowlisted {
            // patternProperties: the comment allowlist accepts any shape.
        } else {
            match schema.get("additionalProperties") {
                Some(serde_json::Value::Bool(false)) | None => {
                    return Err(format!("unexpected property {key}"));
                }
                Some(entry_schema) => {
                    validate(value, entry_schema).map_err(|e| format!("{key}: {e}"))?;
                }
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for name in required {
            let name = name.as_str().expect("required lists strings");
            if !obj.contains_key(name) {
                return Err(format!("missing required property {name}"));
            }
        }
    }
    Ok(())
}

#[test]
fn exported_schema_validates_the_generated_template() {
    let dir = TempDir::new().unwrap();
    let template = builder(parity_schema(), &dir, "app.json")
        .handle_to_string(&ConfigAction::Gen { output: None })
        .unwrap();
    let schema_text = builder(parity_schema(), &dir, "app.json")
        .handle_to_string(&ConfigAction::Schema { output: None })
        .unwrap();

    let instance: serde_json::Value = serde_json::from_str(&template).unwrap();
    let schema: serde_json::Value = serde_json::from_str(&schema_text).unwrap();
    // The template carries comment keys; additionalProperties is false —
    // only the ^// allowlist lets a third-party validator accept it.
    validate(&instance, &schema).unwrap();

    // Cross-check the walk actually rejects a genuinely unknown key.
    let mut broken = instance.clone();
    broken
        .as_object_mut()
        .unwrap()
        .insert("not_in_schema".into(), serde_json::Value::Bool(true));
    assert!(validate(&broken, &schema).is_err());
}
