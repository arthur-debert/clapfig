//! End-to-end tests for `#[derive(clapfig::Schema)]`.
//!
//! These exercise the static-path schema-metadata symmetry contract from
//! `docs/proposals/schema-metadata-symmetry.md`:
//!
//! - JSON Schema `"type"` is emitted for every leaf, including those
//!   without defaults (gap #1).
//! - JSON Schema `"enum"` is emitted for allowed-constrained leaves
//!   (gap #2).
//! - `config gen` template emits `# Allowed: ...` for allowed-constrained
//!   leaves (gap #3) and `#key = <placeholder>` for required leaves
//!   without a default (gap #4).
//!
//! Plus end-to-end load behavior parallels the confique-driven path:
//! defaults, env vars, CLI overrides, strict-mode validation, typed
//! post_validate.

#![cfg(feature = "derive")]

use clapfig::{Clapfig, ConfigAction, ConfigResult, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
struct AppConfig {
    /// Listen host.
    #[clapfig(default = "localhost")]
    host: String,

    /// Listen port.
    #[clapfig(default = 8080)]
    port: u16,

    /// Enable debug mode.
    #[clapfig(default = false)]
    debug: bool,

    /// Database settings.
    database: DbConfig,
}

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
struct DbConfig {
    /// Database URL.
    url: Option<String>,

    /// Connection pool size.
    #[clapfig(default = 5)]
    pool_size: u32,
}

#[test]
fn schema_static_carries_expected_field_names() {
    let s = AppConfig::schema_static();
    assert_eq!(s.name, "AppConfig");
    let names: Vec<&str> = s.fields.iter().map(|f| f.name).collect();
    assert_eq!(names, vec!["host", "port", "debug", "database"]);
}

#[test]
fn schema_runtime_view_matches_static_view() {
    let r = AppConfig::schema();
    assert_eq!(r.name, "AppConfig");
    assert_eq!(r.fields.len(), 4);
}

#[test]
fn load_returns_typed_struct_with_defaults() {
    let dir = TempDir::new().unwrap();
    let cfg: AppConfig = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.host, "localhost");
    assert_eq!(cfg.port, 8080);
    assert!(!cfg.debug);
    assert_eq!(cfg.database.pool_size, 5);
    assert_eq!(cfg.database.url, None);
}

#[test]
fn load_file_overrides_defaults() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("myapp.toml"),
        "host = \"prod.local\"\nport = 9090\n[database]\nurl = \"pg://prod\"\n",
    )
    .unwrap();

    let cfg: AppConfig = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.host, "prod.local");
    assert_eq!(cfg.port, 9090);
    assert_eq!(cfg.database.url.as_deref(), Some("pg://prod"));
}

#[test]
fn strict_rejects_unknown_top_level_key() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("myapp.toml"), "typo_key = 1\n").unwrap();

    let result: Result<AppConfig, _> = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err());
}

#[test]
fn cli_override_wins() {
    let dir = TempDir::new().unwrap();
    let cfg: AppConfig = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .cli_override("port", Some(11111i64))
        .load()
        .unwrap();
    assert_eq!(cfg.port, 11111);
}

#[test]
fn typed_post_validate_sees_merged_c() {
    let dir = TempDir::new().unwrap();
    let result: Result<AppConfig, _> = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .post_validate(|c: &AppConfig| {
            if c.port < 10000 {
                Err(format!("port {} too low", c.port))
            } else {
                Ok(())
            }
        })
        .load();
    let err = result.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("8080") && msg.contains("too low"));
}

// -- Gap #1: JSON Schema "type" emitted for fields without defaults ---------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct RequiredFieldsConfig {
    /// Required name (no default).
    name: String,

    /// Required port (no default).
    port: u32,
}

#[test]
fn json_schema_emits_type_for_required_fields_without_defaults() {
    let result = Clapfig::schema_builder::<RequiredFieldsConfig>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let props = &v["properties"];
    assert_eq!(
        props["name"]["type"], "string",
        "gap #1: required leaf without default must still get a JSON Schema `type`. Got: {props}"
    );
    assert_eq!(props["port"]["type"], "integer", "gap #1 (port)");
}

// -- Gap #2 + #3: enum metadata via `#[clapfig(allowed = [...])]` -----------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct EnumConfig {
    /// Log severity.
    #[clapfig(allowed = ["debug", "info", "warn", "error"], default = "info")]
    level: String,
}

#[test]
fn json_schema_emits_enum_for_allowed_constrained_leaf() {
    let result = Clapfig::schema_builder::<EnumConfig>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let enum_array = v["properties"]["level"]["enum"]
        .as_array()
        .expect("gap #2: allowed-constrained leaf must emit JSON Schema enum");
    let names: Vec<&str> = enum_array.iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(names, vec!["debug", "info", "warn", "error"]);
}

#[test]
fn template_emits_allowed_line_for_enum_leaf() {
    let result = Clapfig::schema_builder::<EnumConfig>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    assert!(
        t.contains("# Allowed: \"debug\" | \"info\" | \"warn\" | \"error\""),
        "gap #3: template must emit `# Allowed:` line. Got:\n{t}"
    );
}

#[test]
fn enum_constraint_rejects_out_of_set_value_at_load() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "level = \"garbage\"\n").unwrap();
    let result: Result<EnumConfig, _> = Clapfig::schema_builder::<EnumConfig>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err());
}

// -- Gap #4: template emits placeholder for required leaves without default -

#[test]
fn template_emits_placeholder_for_required_leaf_without_default() {
    let result = Clapfig::schema_builder::<RequiredFieldsConfig>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    // The runtime-path emitter writes `#key = <placeholder>` for required
    // leaves without a default — `#name = ""` for a String, `#port = 0` for
    // an integer. The confique-driven static path doesn't (gap #4).
    assert!(
        t.contains("#name = \"\""),
        "gap #4: required String leaf must get `#name = \"\"` placeholder. Got:\n{t}"
    );
    assert!(
        t.contains("#port = 0"),
        "gap #4: required integer leaf must get `#port = 0` placeholder. Got:\n{t}"
    );
}

// -- Doc comments propagate to JSON Schema description ---------------------

#[test]
fn doc_comments_become_descriptions() {
    let result = Clapfig::schema_builder::<AppConfig>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let host_desc = v["properties"]["host"]["description"].as_str().unwrap();
    assert!(host_desc.contains("Listen host"));
}

// -- `#[clapfig(env = ...)]` populates the env hint -------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct EnvConfig {
    #[clapfig(env = "X_PORT", default = 1)]
    port: u32,
}

#[test]
fn explicit_env_attribute_is_carried_into_static_schema() {
    let s = EnvConfig::schema_static();
    let leaf = match &s.fields[0].field {
        clapfig::static_schema::FieldStatic::Leaf(l) => l,
        other => panic!("expected Leaf, got {other:?}"),
    };
    assert_eq!(leaf.env, Some("X_PORT"));
}

// -- `#[clapfig(value)]` opt-in to LeafType::Value --------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ValueConfig {
    /// Free-form rule shape.
    #[clapfig(value)]
    rule: toml::Value,
}

#[test]
fn value_attribute_yields_value_leaf_in_static_schema() {
    let s = ValueConfig::schema_static();
    let leaf = match &s.fields[0].field {
        clapfig::static_schema::FieldStatic::Leaf(l) => l,
        other => panic!("expected Leaf, got {other:?}"),
    };
    assert!(matches!(
        leaf.ty,
        clapfig::static_schema::LeafTypeStatic::Value
    ));
}

#[test]
fn value_leaf_accepts_any_shape_at_load() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("test.toml"),
        "rule = [\"warn\", { max = 80 }]\n",
    )
    .unwrap();
    let cfg: ValueConfig = Clapfig::schema_builder::<ValueConfig>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.rule.as_array().is_some());
}

// -- `#[clapfig(rename = "...")]` overrides schema field name ---------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct RenameConfig {
    #[clapfig(rename = "Host", default = "x")]
    #[serde(rename = "Host")]
    host: String,
}

#[test]
fn rename_attribute_changes_schema_field_name() {
    let s = RenameConfig::schema_static();
    assert_eq!(s.fields[0].name, "Host");
}

// -- Struct-level attrs: name override and per-node strict -----------------

#[derive(Schema, Serialize, Deserialize, Debug)]
#[clapfig(name = "RenamedRoot")]
struct NamedRootConfig {
    #[clapfig(default = 1)]
    x: i64,
}

#[test]
fn struct_name_attribute_overrides_schema_name() {
    let s = NamedRootConfig::schema_static();
    assert_eq!(s.name, "RenamedRoot");
}

#[derive(Schema, Serialize, Deserialize, Debug)]
#[clapfig(strict = false)]
struct LenientConfig {
    #[clapfig(default = 1)]
    x: i64,
}

#[test]
fn struct_strict_attribute_cascades_to_unknown_keys() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "x = 2\nrogue = 99\n").unwrap();
    let cfg: LenientConfig = Clapfig::schema_builder::<LenientConfig>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.x, 2);
}

// -- Vec<scalar> support ----------------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct VecConfig {
    #[clapfig(default = ["a", "b"])]
    tags: Vec<String>,
}

#[test]
fn vec_of_string_field_emits_array_leaf_type() {
    let s = VecConfig::schema_static();
    let leaf = match &s.fields[0].field {
        clapfig::static_schema::FieldStatic::Leaf(l) => l,
        other => panic!("expected Leaf, got {other:?}"),
    };
    match &leaf.ty {
        clapfig::static_schema::LeafTypeStatic::Array(inner) => {
            assert!(matches!(
                inner,
                clapfig::static_schema::LeafTypeStatic::String
            ));
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn vec_default_loads_via_runtime_pipeline() {
    let dir = TempDir::new().unwrap();
    let cfg: VecConfig = Clapfig::schema_builder::<VecConfig>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.tags, vec!["a".to_string(), "b".to_string()]);
}

// -- Unit-only enum support (issue #54 item 1) -----------------------------

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[clapfig(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
enum PdfPageSize {
    A4,
    Letter,
    Legal,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct PdfDoc {
    /// Page size for the rendered document.
    page_size: PdfPageSize,
}

#[test]
fn unit_enum_schema_carries_variant_names_post_rename() {
    let s = PdfPageSize::schema_static();
    assert_eq!(s.enum_variants, &["a4", "letter", "legal"]);
    assert!(s.fields.is_empty());
}

#[test]
fn unit_enum_field_flattens_to_runtime_leaf_enum() {
    let s = PdfDoc::schema();
    let leaf = match &s.fields[0].field {
        clapfig::runtime::Field::Leaf(l) => l,
        other => panic!("expected Leaf, got {other:?}"),
    };
    match &leaf.ty {
        clapfig::runtime::LeafType::Enum { values } => {
            assert_eq!(values.len(), 3);
            assert_eq!(values[0], toml::Value::String("a4".into()));
            assert_eq!(values[1], toml::Value::String("letter".into()));
            assert_eq!(values[2], toml::Value::String("legal".into()));
        }
        other => panic!("expected Enum, got {other:?}"),
    }
}

#[test]
fn unit_enum_template_emits_allowed_hint() {
    let result = Clapfig::schema_builder::<PdfDoc>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    assert!(
        t.contains("# Allowed: \"a4\" | \"letter\" | \"legal\""),
        "unit enum must surface allowed hint in template. Got:\n{t}"
    );
}

#[test]
fn unit_enum_load_accepts_known_variant() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "page_size = \"letter\"\n").unwrap();
    let cfg: PdfDoc = Clapfig::schema_builder::<PdfDoc>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.page_size, PdfPageSize::Letter);
}

#[test]
fn unit_enum_load_rejects_unknown_variant() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "page_size = \"a3\"\n").unwrap();
    let result: Result<PdfDoc, _> = Clapfig::schema_builder::<PdfDoc>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err());
}

#[test]
fn unit_enum_json_schema_carries_enum_array() {
    let result = Clapfig::schema_builder::<PdfDoc>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = v["properties"]["page_size"]["enum"]
        .as_array()
        .expect("unit enum field must emit JSON Schema enum array");
    let names: Vec<&str> = arr.iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(names, vec!["a4", "letter", "legal"]);
}

// Enum without rename_all keeps variant names verbatim.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq, Eq)]
enum Mode {
    Fast,
    Slow,
}

#[test]
fn unit_enum_without_rename_all_keeps_pascal_names() {
    let s = Mode::schema_static();
    assert_eq!(s.enum_variants, &["Fast", "Slow"]);
}

// Per-variant `#[clapfig(rename = "...")]` overrides the rename_all rule.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[clapfig(rename_all = "snake_case")]
enum Mixed {
    AlphaBeta,
    #[clapfig(rename = "GAMMA")]
    Gamma,
}

#[test]
fn unit_enum_variant_rename_overrides_rename_all() {
    let s = Mixed::schema_static();
    assert_eq!(s.enum_variants, &["alpha_beta", "GAMMA"]);
}
