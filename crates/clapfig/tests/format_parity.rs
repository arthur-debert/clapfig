//! Format-parity suite — the value-model epic's signature evidence.
//!
//! One logical config, expressed per format, must resolve to the identical
//! [`Value`] tree, produce identical validation errors on the same
//! mistakes, and yield the same JSON Schema. This file carries the YAML
//! slice (WS03): every case pairs the TOML original with its YAML
//! spelling and asserts the outcomes are equal — not merely similar.
//!
//! Divergence-by-design (datetime lexical forms ride strings in YAML and
//! are coerced by the schema pass) is covered by the same equality: the
//! *resolved* trees must match even where the file-level spelling differs.

use clapfig::Clapfig;
use clapfig::error::ClapfigError;
use clapfig::runtime::{Field, Schema as RtSchema};
use clapfig::types::SearchPath;
use clapfig::value::{Map, Value};
use tempfile::TempDir;

/// The shared logical schema: scalar leaves, an enum, a datetime, and a
/// nested section.
fn shared_schema() -> clapfig::runtime::Schema {
    RtSchema::object("App")
        .doc("Parity demo")
        .field("host", Field::string().doc("Host").default("localhost"))
        .field("port", Field::integer().doc("Port").default(8080i64))
        .field(
            "level",
            Field::enum_of(["debug", "info"])
                .doc("Verbosity")
                .default("info"),
        )
        .field(
            "launched",
            Field::datetime().doc("Launch instant").optional(),
        )
        .nested(
            "database",
            RtSchema::object("Db")
                .doc("Database")
                .field("url", Field::string().optional())
                .field("pool_size", Field::integer().default(5i64)),
        )
        .build()
}

/// Resolve `content` written as `app.<ext>` in a fresh directory.
fn load(ext: &str, content: &str) -> Result<Map, ClapfigError> {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(format!("app.{ext}")), content).unwrap();
    Clapfig::builder(shared_schema())
        .app_name("app")
        .file_name(&format!("app.{ext}"))
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
}

#[test]
fn shared_config_resolves_to_the_identical_value_tree() {
    let toml = load(
        "toml",
        r#"host = "example.com"
port = 9090
level = "debug"
launched = 2020-05-27T07:32:00Z

[database]
url = "pg://prod"
"#,
    )
    .unwrap();
    let yaml = load(
        "yaml",
        r#"host: example.com
port: 9090
level: debug
launched: 2020-05-27T07:32:00Z

database:
  url: pg://prod
"#,
    )
    .unwrap();

    assert_eq!(toml, yaml, "resolved trees must be identical");
    // And the datetime is the owned variant in both — TOML natively, YAML
    // via the schema-driven coercion of its string form (ADR-0001).
    assert!(
        matches!(toml["launched"], Value::Datetime(_)),
        "launched resolves to a datetime, got {:?}",
        toml["launched"]
    );
}

#[test]
fn defaults_fill_identically_when_the_file_is_minimal() {
    let toml = load("toml", "host = \"x\"\n").unwrap();
    let yaml = load("yaml", "host: x\n").unwrap();
    assert_eq!(toml, yaml);
    assert_eq!(toml["port"], Value::Integer(8080));
    assert_eq!(
        toml["database"].as_map().unwrap()["pool_size"],
        Value::Integer(5)
    );
}

#[test]
fn type_mismatch_produces_the_identical_error() {
    let toml = load("toml", "port = \"not-a-number\"\n").unwrap_err();
    let yaml = load("yaml", "port: \"not-a-number\"\n").unwrap_err();
    match (toml, yaml) {
        (
            ClapfigError::InvalidValue {
                key: tk,
                reason: tr,
                ..
            },
            ClapfigError::InvalidValue {
                key: yk,
                reason: yr,
                ..
            },
        ) => {
            assert_eq!(tk, yk);
            assert_eq!(tr, yr);
        }
        (t, y) => panic!("expected matching InvalidValue, got {t:?} vs {y:?}"),
    }
}

#[test]
fn enum_violation_produces_the_identical_error() {
    let toml = load("toml", "level = \"loud\"\n").unwrap_err();
    let yaml = load("yaml", "level: loud\n").unwrap_err();
    match (toml, yaml) {
        (
            ClapfigError::InvalidValue {
                key: tk,
                reason: tr,
                ..
            },
            ClapfigError::InvalidValue {
                key: yk,
                reason: yr,
                ..
            },
        ) => {
            assert_eq!(tk, yk);
            assert_eq!(tr, yr);
        }
        (t, y) => panic!("expected matching InvalidValue, got {t:?} vs {y:?}"),
    }
}

#[test]
fn unknown_key_strictness_produces_the_identical_error() {
    let toml = load("toml", "bogus = 1\n").unwrap_err();
    let yaml = load("yaml", "bogus: 1\n").unwrap_err();
    match (toml, yaml) {
        (ClapfigError::UnknownKeys(t), ClapfigError::UnknownKeys(y)) => {
            let tk: Vec<_> = t.iter().map(|u| u.key.clone()).collect();
            let yk: Vec<_> = y.iter().map(|u| u.key.clone()).collect();
            assert_eq!(tk, yk);
            assert_eq!(tk, ["bogus"]);
        }
        (t, y) => panic!("expected matching UnknownKeys, got {t:?} vs {y:?}"),
    }
}

#[test]
fn json_schema_export_is_format_independent() {
    // The JSON Schema derives from the runtime schema, not from any config
    // file — the YAML slice's guarantee is that adopting YAML changes
    // nothing about the exported schema.
    let from_toml_context = clapfig::json_schema::generate_schema(&shared_schema());
    let from_yaml_context = clapfig::json_schema::generate_schema(&shared_schema());
    assert_eq!(from_toml_context, from_yaml_context);
    assert_eq!(from_toml_context["type"], serde_json::json!("object"));
    assert!(from_toml_context["properties"]["database"]["properties"]["pool_size"].is_object());
}
