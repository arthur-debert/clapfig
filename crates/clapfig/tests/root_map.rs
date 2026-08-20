//! SHP01-WS03: a homogeneous Map as the document root.
//!
//! Runtime builder and typed `BTreeMap`/`HashMap` are twins: `[core]` /
//! `[site]` load with no parent field; unknown keys, missing required
//! fields, and type errors name origin; JSON Schema is
//! `additionalProperties` at the root; templates show a commented example
//! entry; `config set` of a dynamic key refuses with `UnaddressableKey`.

#![cfg(feature = "derive")]

use std::collections::{BTreeMap, HashMap};
use std::fs;

use clapfig::runtime::{Field, Schema, Shape};
use clapfig::{Clapfig, ConfigAction, ConfigResult, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

#[derive(clapfig::Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Site {
    /// Bind address.
    host: String,
    /// Listen port.
    #[clapfig(default = 8080)]
    port: u16,
}

fn item_object() -> Schema {
    Schema::object("Site")
        .field("host", Field::string())
        .field("port", Field::integer().default(8080i64))
        .build()
}

fn root_map() -> Shape {
    Shape::from(Shape::map("sites", item_object()).build())
}

fn write_sites(dir: &TempDir) {
    fs::write(
        dir.path().join("demo.toml"),
        "[core]\nhost = \"a.example\"\n\n[site]\nhost = \"b.example\"\nport = 9090\n",
    )
    .unwrap();
}

fn load_builder(dir: &TempDir) -> clapfig::value::Map {
    Clapfig::builder(root_map())
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap()
}

#[test]
fn runtime_root_map_loads_named_entries_with_no_parent_field() {
    let dir = TempDir::new().unwrap();
    write_sites(&dir);
    let table = load_builder(&dir);
    assert!(
        !table.contains_key("sites"),
        "root map must not invent a parent field: {table:?}"
    );
    assert_eq!(table["core"]["host"].as_str().unwrap(), "a.example");
    assert_eq!(table["core"]["port"].as_integer().unwrap(), 8080);
    assert_eq!(table["site"]["host"].as_str().unwrap(), "b.example");
    assert_eq!(table["site"]["port"].as_integer().unwrap(), 9090);
}

#[test]
fn typed_btreemap_and_hashmap_produce_the_same_load() {
    let dir = TempDir::new().unwrap();
    write_sites(&dir);

    let via_btree: BTreeMap<String, Site> = Clapfig::typed::<BTreeMap<String, Site>>()
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    let via_hash: HashMap<String, Site> = Clapfig::typed::<HashMap<String, Site>>()
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();

    assert_eq!(via_btree.len(), 2);
    assert_eq!(via_hash.len(), 2);
    assert_eq!(via_btree["core"].host, "a.example");
    assert_eq!(via_hash["core"].host, "a.example");
    assert_eq!(via_btree["core"].port, 8080);
    assert_eq!(via_hash["site"].port, 9090);
}

#[test]
fn root_map_unknown_key_names_origin() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("demo.toml"),
        "[core]\nhost = \"a.example\"\nrogue = 1\n",
    )
    .unwrap();
    let err = Clapfig::builder(root_map())
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(true)
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("expected UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "core.rogue");
    assert_eq!(keys[0].path, dir.path().join("demo.toml"));
    assert!(keys[0].line > 0, "unknown key must name a line: {keys:?}");
}

#[test]
fn root_map_missing_required_field_has_discovery_not_origin() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("demo.toml"), "[core]\nport = 1\n").unwrap();
    let err = Clapfig::builder(root_map())
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap_err();
    match err {
        clapfig::ClapfigError::MissingRequired { key, discovery } => {
            assert_eq!(key, "core.host");
            assert_eq!(discovery.files.len(), 1);
            let msg = clapfig::ClapfigError::MissingRequired { key, discovery }.to_string();
            assert!(
                !msg.contains("set by"),
                "MissingRequired must not name a winning origin: {msg}"
            );
        }
        other => panic!("expected MissingRequired, got {other:?}"),
    }
}

#[test]
fn root_map_type_error_names_origin() {
    let dir = TempDir::new().unwrap();
    let source = "[core]\nhost = 1\n";
    fs::write(dir.path().join("demo.toml"), source).unwrap();
    let err = Clapfig::builder(root_map())
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap_err();
    match err {
        clapfig::ClapfigError::InvalidValue {
            key,
            reason,
            origin,
            ..
        } => {
            assert_eq!(key, "core.host");
            assert!(reason.contains("expected string"), "{reason}");
            assert_eq!(
                origin.file.as_deref(),
                Some(dir.path().join("demo.toml").as_path())
            );
            assert_eq!(origin.input_type, Some(clapfig::InputType::File));
            let span = origin.span.expect("value span");
            let text = origin.source.as_deref().expect("retained source");
            assert_eq!(&text[span.start..span.end], "1");
        }
        other => panic!("expected InvalidValue, got {other:?}"),
    }
}

#[test]
fn json_schema_additional_properties_at_document_root() {
    let value = clapfig::json_schema::generate_from_shape(&root_map());
    assert_eq!(value["type"], "object");
    assert!(
        value.get("properties").is_none(),
        "root map must not invent a parent property: {value}"
    );
    let additional = &value["additionalProperties"];
    assert_eq!(additional["type"], "object");
    assert_eq!(additional["title"], "Site");
    assert_eq!(additional["properties"]["host"]["type"], "string");
}

#[test]
fn template_has_commented_example_entry_not_parent_table() {
    let result = Clapfig::builder(root_map())
        .app_name("demo")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    match result {
        ConfigResult::Template(text) => {
            assert!(
                !text.contains("[sites]"),
                "must not invent a parent table:\n{text}"
            );
            assert!(
                text.contains("[<key>]") || text.contains("#[<key>]"),
                "expected a commented example entry:\n{text}"
            );
            assert!(
                text.lines().any(|l| l.trim_start().starts_with('#')),
                "example entry must be commented:\n{text}"
            );
        }
        other => panic!("expected Template, got {other:?}"),
    }
}

#[test]
fn object_root_template_unchanged() {
    let object = Schema::object("App")
        .field("host", Field::string().default("localhost"))
        .build();
    let as_schema = Clapfig::builder(object.clone())
        .app_name("demo")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let as_shape = Clapfig::builder(Shape::Object(object))
        .app_name("demo")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    match (as_schema, as_shape) {
        (ConfigResult::Template(a), ConfigResult::Template(b)) => assert_eq!(a, b),
        other => panic!("expected templates, got {other:?}"),
    }
}

#[test]
fn config_set_of_dynamic_entry_key_refuses() {
    let dir = TempDir::new().unwrap();
    let err = Clapfig::builder(root_map())
        .app_name("demo")
        .file_name("demo.toml")
        .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
        .no_env()
        .handle(&ConfigAction::Set {
            key: "core.host".into(),
            value: "x".into(),
            scope: None,
        })
        .unwrap_err();
    match err {
        clapfig::ClapfigError::UnaddressableKey { key, kind, .. } => {
            assert_eq!(key, "core.host");
            assert_eq!(kind, "a map");
        }
        other => panic!("expected UnaddressableKey, got {other:?}"),
    }
}

fn nested_strict_root(db_strict: bool) -> Shape {
    let item = Schema::object("Site")
        .field("host", Field::string().default("localhost"))
        .nested(
            "db",
            Schema::object("Db")
                .strict(db_strict)
                .field("url", Field::string().optional()),
        );
    Shape::from(Shape::map("sites", item))
}

#[test]
fn template_root_map_of_leaves_is_commented_assignment() {
    let result = Clapfig::builder(Shape::from(Shape::map("values", Field::string())))
        .app_name("demo")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    match result {
        ConfigResult::Template(text) => {
            assert!(
                text.contains("#<key> = \"\""),
                "value-shaped root map must be a commented assignment:\n{text}"
            );
            assert!(
                !text.contains("[<key>]") && !text.contains("[[<key>]]"),
                "must not emit a table header for a leaf item:\n{text}"
            );
        }
        other => panic!("expected Template, got {other:?}"),
    }
}

#[test]
fn root_map_nested_strict_false_accepts_unknown_under_every_file_entry() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("demo.toml"),
        "[core.db]\nurl = \"a\"\nrogue = 1\n\n[site.db]\nurl = \"b\"\nrogue = 2\n",
    )
    .unwrap();
    let table = Clapfig::builder(nested_strict_root(false))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(true)
        .load()
        .unwrap();
    assert_eq!(table["core"]["db"]["url"].as_str().unwrap(), "a");
    assert_eq!(table["site"]["db"]["url"].as_str().unwrap(), "b");
}

#[test]
fn root_map_nested_strict_false_still_rejects_unknown_at_entry_top_level() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("demo.toml"),
        "[core]\nhost = \"a\"\nextra = 1\n",
    )
    .unwrap();
    let err = Clapfig::builder(nested_strict_root(false))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(true)
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("expected UnknownKeys");
    assert_eq!(keys[0].key, "core.extra");
}

#[test]
fn root_map_entry_named_like_nested_strict_path_does_not_steal_override() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("demo.toml"), "[db]\nrogue = 1\n").unwrap();
    let err = Clapfig::builder(nested_strict_root(false))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(true)
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("expected UnknownKeys");
    assert_eq!(
        keys[0].key, "db.rogue",
        "entry named db must not inherit item db.strict(false): {keys:?}"
    );
}

#[test]
fn root_map_nested_strict_true_rejects_unknown_under_every_file_entry() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("demo.toml"),
        "[core.db]\nurl = \"a\"\nrogue = 1\n\n[site.db]\nurl = \"b\"\nrogue = 2\n",
    )
    .unwrap();
    let err = Clapfig::builder(nested_strict_root(true))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(false)
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("expected UnknownKeys");
    let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
    assert!(
        names.contains(&"core.db.rogue") && names.contains(&"site.db.rogue"),
        "both entries must reject under db.strict(true): {names:?}"
    );
}

#[test]
fn root_map_nested_strict_false_accepts_unknown_under_every_env_entry() {
    const PREFIX: &str = "CLAPFIG_SHP01_WS03_ROOT_MAP_STRICT_OK";
    let core_db = format!("{PREFIX}__CORE__DB__ROGUE");
    let site_db = format!("{PREFIX}__SITE__DB__ROGUE");
    unsafe {
        std::env::set_var(&core_db, "1");
        std::env::set_var(&site_db, "1");
    }

    let dir = TempDir::new().unwrap();
    let table = Clapfig::builder(nested_strict_root(false))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .env_prefix(PREFIX)
        .strict(true)
        .load();

    unsafe {
        std::env::remove_var(&core_db);
        std::env::remove_var(&site_db);
    }

    table.expect("db.strict(false) must drop env unknowns under every map entry");
}

#[test]
fn root_map_nested_strict_true_rejects_unknown_under_every_env_entry() {
    const PREFIX: &str = "CLAPFIG_SHP01_WS03_ROOT_MAP_STRICT_NO";
    let core_db = format!("{PREFIX}__CORE__DB__ROGUE");
    let site_db = format!("{PREFIX}__SITE__DB__ROGUE");
    unsafe {
        std::env::set_var(&core_db, "1");
        std::env::set_var(&site_db, "1");
    }

    let dir = TempDir::new().unwrap();
    let result = Clapfig::builder(nested_strict_root(true))
        .app_name("demo")
        .file_name("demo.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .env_prefix(PREFIX)
        .strict(false)
        .load();

    unsafe {
        std::env::remove_var(&core_db);
        std::env::remove_var(&site_db);
    }

    let err = result.expect_err("db.strict(true) must reject env unknowns");
    let keys = err.unknown_keys().expect("expected UnknownKeys");
    let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
    assert!(
        names.contains(&"core.db.rogue") && names.contains(&"site.db.rogue"),
        "both env entries must reject under db.strict(true): {names:?}"
    );
}
