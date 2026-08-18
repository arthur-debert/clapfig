//! Regression tests for the DER01-WS02 derive-hardening pass (#131): the
//! silent-wrong behaviors that *compile* — so trybuild can't cover them —
//! and now must produce a schema that agrees with serde.
//!
//! Covered here:
//! - `HashMap<String, UnitEnum>` flattens to `Map(Enum)` instead of an
//!   empty `MapOf` schema that rejected every entry at load.
//! - Absent derived maps (flattened enum maps, bare scalar maps, and
//!   structural map-of-struct) load as the empty map; `Option<Map<..>>`
//!   stays `None`. Map leaves are not `required` in JSON Schema.
//! - Raw identifiers (`r#type`) emit serde's spelling (`type`).
//! - Field-site `///` docs are retained on bare nested / enum / map-of
//!   fields (previously dropped, asymmetric with `Option<Enum>`).
//! - Enum-typed field defaults are checked against the variant set at
//!   first `schema()` call.
//! - `Vec<Datetime>` array defaults emit datetime-typed elements.
//!
//! The compile-time rejections added by the same pass live in
//! `tests/ui/derive/`.

#![cfg(feature = "derive")]

use std::collections::HashMap;

use clapfig::runtime::{Field as RuntimeField, LeafType as RuntimeLeafType};
use clapfig::value::Value;
use clapfig::{Clapfig, ConfigAction, ConfigResult, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

// -- HashMap<String, UnitEnum> flattens to Map(Enum) ------------------------

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Level {
    Debug,
    Info,
    Warn,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct PerTargetLevels {
    /// Per-target log level.
    levels: HashMap<String, Level>,
}

#[test]
fn map_of_unit_enum_flattens_to_map_of_enum_leaf() {
    let s = PerTargetLevels::schema();
    match &s.fields[0].field {
        RuntimeField::Leaf(leaf) => match &leaf.ty {
            RuntimeLeafType::Map(inner) => match inner.as_ref() {
                RuntimeLeafType::Enum { values } => {
                    assert_eq!(
                        values,
                        &vec![
                            Value::String("debug".into()),
                            Value::String("info".into()),
                            Value::String("warn".into()),
                        ]
                    );
                }
                other => panic!("expected Enum inside Map, got {other:?}"),
            },
            other => panic!("expected Map leaf type, got {other:?}"),
        },
        other => panic!("expected Leaf (map-of-enum flattened), got {other:?}"),
    }
}

#[test]
fn map_of_unit_enum_loads_string_entries() {
    // The old `MapOf(Schema { fields: [] })` shape failed every entry at
    // load with "expected map, got string".
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "[levels]\ncore = \"debug\"\nnet = \"warn\"\n",
    )
    .unwrap();
    let cfg: PerTargetLevels = Clapfig::typed::<PerTargetLevels>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.levels["core"], Level::Debug);
    assert_eq!(cfg.levels["net"], Level::Warn);
}

#[test]
fn map_of_unit_enum_rejects_out_of_set_entry_at_load() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "[levels]\ncore = \"loud\"\n").unwrap();
    let result: Result<PerTargetLevels, _> = Clapfig::typed::<PerTargetLevels>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err(), "out-of-set enum entry must fail the load");
}

#[test]
fn map_of_unit_enum_contributes_a_single_field_path() {
    assert_eq!(PerTargetLevels::field_paths(), vec!["levels".to_string()]);
}

// -- Raw identifiers unraw to serde's spelling ------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct RawIdent {
    /// Entry type.
    #[clapfig(default = "file")]
    r#type: String,
}

#[test]
fn raw_identifier_field_emits_unraw_schema_name() {
    // serde matches `r#type` against the key "type"; the schema must
    // carry the same spelling or the config key would be rejected as
    // unknown while serde expects it.
    assert_eq!(<RawIdent as Schema>::STATIC.fields[0].name, "type");
}

#[test]
fn raw_identifier_field_loads_from_its_serde_spelling() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "type = \"dir\"\n").unwrap();
    let cfg: RawIdent = Clapfig::typed::<RawIdent>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.r#type, "dir");
}

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
enum RawVariant {
    r#Try,
    Catch,
}

#[test]
fn raw_identifier_variant_emits_unraw_value() {
    assert_eq!(
        <RawVariant as Schema>::STATIC.enum_variants,
        &["Try", "Catch"]
    );
}

// -- Field-site docs on bare nested / enum / map-of fields ------------------

/// Type-level doc on the nested struct.
#[derive(Schema, Serialize, Deserialize, Debug)]
struct Inner {
    /// Port.
    #[clapfig(default = 1)]
    port: u16,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct DocCarrier {
    /// Primary database connection.
    db: Inner,

    /// Rendering mode for the main view.
    mode: Level,

    /// Per-plugin settings, keyed by plugin name.
    plugins: HashMap<String, Inner>,
}

#[test]
fn field_site_docs_survive_on_nested_enum_and_map_of_fields() {
    // Previously the `Nested`/`MapOf` emission dropped the field's `///`
    // doc (only `Option<Enum>` kept it); the field-site doc now wins over
    // the referenced type's own doc.
    let s = DocCarrier::schema();
    match &s.fields[0].field {
        RuntimeField::Nested(inner) => {
            assert_eq!(inner.doc, vec!["Primary database connection.".to_string()]);
        }
        other => panic!("expected Nested, got {other:?}"),
    }
    match &s.fields[1].field {
        RuntimeField::Leaf(leaf) => {
            assert_eq!(
                leaf.doc,
                vec!["Rendering mode for the main view.".to_string()]
            );
        }
        other => panic!("expected Leaf (enum flattened), got {other:?}"),
    }
    match &s.fields[2].field {
        RuntimeField::MapOf(entry) => {
            assert_eq!(
                entry.doc,
                vec!["Per-plugin settings, keyed by plugin name.".to_string()]
            );
        }
        other => panic!("expected MapOf, got {other:?}"),
    }
}

#[test]
fn nested_field_without_field_doc_falls_back_to_type_doc() {
    #[derive(Schema, Serialize, Deserialize, Debug)]
    struct NoFieldDoc {
        db: Inner,
    }
    let s = NoFieldDoc::schema();
    match &s.fields[0].field {
        RuntimeField::Nested(inner) => {
            assert_eq!(
                inner.doc,
                vec!["Type-level doc on the nested struct.".to_string()]
            );
        }
        other => panic!("expected Nested, got {other:?}"),
    }
}

// -- Enum-typed field defaults are variant-checked --------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct BadEnumDefault {
    /// The variant set lives on `Level`, which the macro can't see —
    /// membership is checked at the first `schema()` call instead.
    #[clapfig(default = "verbose")]
    level: Level,
}

#[test]
#[should_panic(expected = "not a variant of enum `Level`")]
fn enum_typed_field_default_outside_variant_set_panics_at_schema_call() {
    let _ = BadEnumDefault::schema();
}

// -- Vec<Datetime> array defaults emit datetime elements --------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct DatetimeArray {
    /// Maintenance windows.
    #[clapfig(default = ["2020-01-01T00:00:00Z"])]
    windows: Vec<clapfig::value::Datetime>,
}

// -- Absent maps load as the empty map ---------------------------------------
//
// Map entries are user-supplied, so absence means "no entries" — the rule
// the structural `MapOf` shape always had. Derived map *leaves* (the
// `Map(Enum)` flatten and bare scalar maps) must follow it too: no
// `MissingRequired` at validation, no missing-field error at the typed
// serde deserialize, and no `required` listing in JSON Schema.

#[test]
fn absent_map_of_unit_enum_loads_as_empty_map() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: PerTargetLevels = Clapfig::typed::<PerTargetLevels>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.levels.is_empty());
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ScalarMapCfg {
    /// Named limits.
    limits: HashMap<String, i64>,
}

#[test]
fn absent_bare_scalar_map_loads_as_empty_map() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: ScalarMapCfg = Clapfig::typed::<ScalarMapCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.limits.is_empty());
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct StructuralMapCfg {
    /// Per-plugin settings.
    plugins: HashMap<String, Inner>,
}

#[test]
fn absent_structural_map_of_loads_as_empty_map() {
    // The structural shape always accepted absence at validation, but the
    // typed path still needs `fill_defaults` to materialize the `{}` or
    // serde fails with a missing-field error.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: StructuralMapCfg = Clapfig::typed::<StructuralMapCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.plugins.is_empty());
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct OptScalarMapCfg {
    /// Optional labels.
    labels: Option<HashMap<String, String>>,
}

#[test]
fn absent_optional_map_stays_none() {
    // `Option<Map<..>>` keeps the presence signal: no empty-map default is
    // synthesized, so absence deserializes to `None`, not `Some({})`.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: OptScalarMapCfg = Clapfig::typed::<OptScalarMapCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.labels.is_none());
}

#[test]
fn map_leaf_is_not_required_in_json_schema() {
    // Mirrors the structural-MapOf rule: an absent map loads as the empty
    // map, so a JSON Schema requiring the key would reject configs clapfig
    // accepts.
    let result = Clapfig::typed::<PerTargetLevels>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(
        v.get("required").is_none(),
        "map leaf must not be listed as required, got: {:?}",
        v["required"]
    );
    assert_eq!(v["properties"]["levels"]["type"], "object");
}

#[test]
fn vec_datetime_array_default_emits_datetime_elements() {
    // Elements used to emit `ValueStatic::String`, so the datetime-typed
    // leaf rejected its own default at finalize.
    let s = DatetimeArray::schema();
    match &s.fields[0].field {
        RuntimeField::Leaf(leaf) => match leaf.default.as_ref().unwrap() {
            Value::Array(items) => {
                assert!(matches!(items[0], Value::Datetime(_)));
            }
            other => panic!("expected Array default, got {other:?}"),
        },
        other => panic!("expected Leaf, got {other:?}"),
    }
}
