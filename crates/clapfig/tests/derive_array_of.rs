//! End-to-end tests for `Vec<T>` derive emission where `T` derives
//! `clapfig::Schema` (DER01-WS03, tracking #106).
//!
//! - `Vec<NestedStruct>` emits the structural `Field::ArrayOf` (TOML
//!   `[[name]]`), with per-entry defaults, per-entry required/type checks
//!   with indexed paths, strict unknown-key detection inside entries, and
//!   typed loading into `Vec<T>`.
//! - `Vec<UnitEnum>` flattens to an `Array(Enum)` *leaf* — the array
//!   sibling of the WS02 `Map(Enum)` flatten — so entries validate against
//!   the variant set and surfaces (template `Allowed:`, JSON Schema
//!   `items.enum`) carry the per-item value set.
//! - Absence rule: an absent array is the empty array (mirroring the
//!   map rule) — bare `Vec<..>` fields load as empty, `Option<Vec<..>>`
//!   keeps the presence signal and loads as `None`.
//! - `Option<Vec<Struct>>` has no representation; it panics at the first
//!   `schema()` call with drop-the-`Option` guidance (deferred authoring
//!   error — the macro cannot tell enum from struct syntactically).

#![cfg(feature = "derive")]

use clapfig::runtime::Shape as RuntimeShape;
use clapfig::value::Value;
use clapfig::{Clapfig, ConfigAction, ConfigResult, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

// -- Vec<NestedStruct> → structural ArrayOf ---------------------------------

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
struct Plugin {
    /// Plugin name.
    name: String,
    /// Load priority.
    #[clapfig(default = 10)]
    priority: i64,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AppCfg {
    /// Installed plugins.
    plugins: Vec<Plugin>,
}

#[test]
fn vec_of_struct_emits_structural_array_of_with_field_site_doc() {
    let s = AppCfg::schema();
    match &s.fields[0].field {
        RuntimeShape::Array(array) => match array.item.as_ref() {
            RuntimeShape::Object(item) => {
                assert_eq!(item.name, "Plugin");
                // Field-site `///` doc wins over the item type's own doc.
                assert_eq!(item.doc, vec!["Installed plugins.".to_string()]);
            }
            other => panic!("expected Object item, got {other:?}"),
        },
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn vec_of_struct_loads_typed_entries_with_per_entry_defaults() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "[[plugins]]\nname = \"audit\"\n[[plugins]]\nname = \"lint\"\npriority = 1\n",
    )
    .unwrap();
    let cfg: AppCfg = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(
        cfg.plugins,
        vec![
            Plugin {
                name: "audit".into(),
                priority: 10, // per-entry default filled in
            },
            Plugin {
                name: "lint".into(),
                priority: 1,
            },
        ]
    );
}

#[test]
fn vec_of_struct_empty_array_loads_empty() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "plugins = []\n").unwrap();
    let cfg: AppCfg = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.plugins.is_empty());
}

#[test]
fn absent_vec_of_struct_loads_as_empty_vec() {
    // The structural shape accepts absence at validation, and the typed
    // path needs `fill_defaults` to materialize the `[]` or serde fails
    // with a missing-field error (same rule as absent `MapOf` → `{}`).
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: AppCfg = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.plugins.is_empty());
}

#[test]
fn vec_of_struct_missing_required_entry_field_errors_with_indexed_path() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "[[plugins]]\npriority = 3\n").unwrap();
    let err = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .map(|_: AppCfg| ())
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("plugins[0].name"),
        "missing required entry field must be reported with an indexed path, got: {msg}"
    );
}

#[test]
fn vec_of_struct_entry_type_error_carries_indexed_path() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "[[plugins]]\nname = \"a\"\n[[plugins]]\nname = \"b\"\npriority = \"high\"\n",
    )
    .unwrap();
    let err = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .map(|_: AppCfg| ())
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("plugins[1].priority"),
        "entry type error must be reported with an indexed path, got: {msg}"
    );
}

#[test]
fn strict_validation_flags_unknown_entry_key_with_indexed_path() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "[[plugins]]\nname = \"a\"\nrogue = 1\n",
    )
    .unwrap();
    let err = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .map(|_: AppCfg| ())
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("plugins[0].rogue"),
        "unknown entry key must be flagged with an indexed path, got: {msg}"
    );
}

#[test]
fn vec_of_struct_field_paths_include_section_and_children() {
    assert_eq!(
        AppCfg::field_paths(),
        vec![
            "plugins".to_string(),
            "plugins.name".to_string(),
            "plugins.priority".to_string(),
        ]
    );
}

#[test]
fn vec_of_struct_json_schema_emits_items_schema_and_is_not_required() {
    let result = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let plugins = &v["properties"]["plugins"];
    assert_eq!(plugins["type"], "array");
    assert_eq!(plugins["items"]["properties"]["name"]["type"], "string");
    // An absent array loads as the empty array, so the property must not
    // be listed as required.
    let required: Vec<&str> = v["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    assert!(
        !required.contains(&"plugins"),
        "array-of property must not be required, got: {required:?}"
    );
}

#[test]
fn vec_of_struct_template_renders_commented_array_of_tables_example() {
    let result = Clapfig::typed::<AppCfg>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    assert!(
        t.contains("#[[plugins]]"),
        "template must emit a commented array-of-tables example, got:\n{t}"
    );
}

// -- Vec<UnitEnum> → Array(Enum) leaf flatten -------------------------------

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum PageSize {
    A4,
    Letter,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct PrintCfg {
    /// Accepted page sizes.
    sizes: Vec<PageSize>,
}

#[test]
fn vec_of_unit_enum_flattens_to_array_of_enum_leaf() {
    let s = PrintCfg::schema();
    match &s.fields[0].field {
        RuntimeShape::Array(array) => match array.item.as_ref() {
            RuntimeShape::Leaf(leaf) => match &leaf.ty {
                clapfig::runtime::LeafType::Enum { values } => {
                    assert_eq!(
                        values,
                        &vec![Value::String("a4".into()), Value::String("letter".into())]
                    );
                }
                other => panic!("expected Enum inside Array, got {other:?}"),
            },
            other => panic!("expected Leaf item, got {other:?}"),
        },
        other => panic!("expected Array (array-of-enum flattened), got {other:?}"),
    }
}

#[test]
fn vec_of_unit_enum_loads_typed_entries() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "sizes = [\"letter\", \"a4\"]\n").unwrap();
    let cfg: PrintCfg = Clapfig::typed::<PrintCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.sizes, vec![PageSize::Letter, PageSize::A4]);
}

#[test]
fn vec_of_unit_enum_rejects_out_of_set_entry_at_load() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "sizes = [\"a4\", \"tabloid\"]\n").unwrap();
    let result: Result<PrintCfg, _> = Clapfig::typed::<PrintCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err(), "out-of-set enum entry must fail the load");
}

#[test]
fn absent_vec_of_unit_enum_loads_as_empty_vec() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: PrintCfg = Clapfig::typed::<PrintCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.sizes.is_empty());
}

#[test]
fn vec_of_unit_enum_contributes_a_single_field_path() {
    assert_eq!(PrintCfg::field_paths(), vec!["sizes".to_string()]);
}

#[test]
fn vec_of_unit_enum_json_schema_emits_items_enum_and_is_not_required() {
    let result = Clapfig::typed::<PrintCfg>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let sizes = &v["properties"]["sizes"];
    assert_eq!(sizes["type"], "array");
    let items_enum: Vec<&str> = sizes["items"]["enum"]
        .as_array()
        .expect("array-of-enum leaf must emit `enum` on the items schema")
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert_eq!(items_enum, vec!["a4", "letter"]);
    let required: Vec<&str> = v["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    assert!(!required.contains(&"sizes"));
}

#[test]
fn vec_of_unit_enum_template_emits_allowed_line() {
    let result = Clapfig::typed::<PrintCfg>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    assert!(
        t.contains("# Allowed: \"a4\" | \"letter\""),
        "array-of-enum leaf must list its per-item value set, got:\n{t}"
    );
    assert!(
        !t.contains("Required."),
        "array leaf materializes as [] when absent, so the template must not mark it required, got:\n{t}"
    );
}

// -- Option<Vec<UnitEnum>> keeps the presence signal ------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct OptSizes {
    /// Optional page-size override list.
    sizes: Option<Vec<PageSize>>,
}

#[test]
fn optional_vec_of_unit_enum_absent_stays_none() {
    // `Option<Vec<..>>` keeps the presence signal: no empty-array default
    // is synthesized, so absence deserializes to `None`, not `Some([])`.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: OptSizes = Clapfig::typed::<OptSizes>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.sizes.is_none());
}

#[test]
fn optional_vec_of_unit_enum_present_loads_some() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "sizes = [\"a4\"]\n").unwrap();
    let cfg: OptSizes = Clapfig::typed::<OptSizes>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.sizes, Some(vec![PageSize::A4]));
}

#[test]
fn optional_vec_of_unit_enum_rejects_out_of_set_entry() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "sizes = [\"tabloid\"]\n").unwrap();
    let result: Result<OptSizes, _> = Clapfig::typed::<OptSizes>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err());
}

// -- Option<Vec<Struct>> is a deferred authoring error ----------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct OptPlugins {
    /// No representation: absence is already the empty array.
    plugins: Option<Vec<Plugin>>,
}

#[test]
#[should_panic(expected = "is a struct, not a unit-only enum")]
fn optional_vec_of_struct_panics_with_drop_option_guidance() {
    // The macro can't tell enum from struct at the field site, so
    // `Option<Vec<T>>` routes through `Array(EnumRef)` and the struct
    // kind fails at the first `schema()` call — same deferred-error
    // pattern as `Option<NestedStruct>`.
    let _ = OptPlugins::schema();
}

// -- Absence rule extends to scalar array leaves ----------------------------
//
// The `Vec<UnitEnum>` flatten produces a plain `Array(Enum)` leaf the
// walkers cannot tell apart from a builder-built or `Vec<scalar>` array
// leaf, so array leaves as a class follow the array absence rule (exactly
// how map leaves follow `MapOf`'s): absent non-optional array leaves
// without a declared default load as the empty array.

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ScalarVecCfg {
    /// Free-form tags.
    tags: Vec<String>,
}

#[test]
fn absent_bare_scalar_vec_loads_as_empty_vec() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: ScalarVecCfg = Clapfig::typed::<ScalarVecCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.tags.is_empty());
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct DefaultedVecCfg {
    /// Tags with an explicit default.
    #[clapfig(default = ["a", "b"])]
    tags: Vec<String>,
}

#[test]
fn declared_array_default_wins_over_empty_materialization() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "").unwrap();
    let cfg: DefaultedVecCfg = Clapfig::typed::<DefaultedVecCfg>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.tags, vec!["a".to_string(), "b".to_string()]);
}
