//! SHP01-WS05: typed tagged unions and honest artifacts.
//!
//! `#[serde(tag = "kind")]` enums derive Schema, load, typed-deserialize,
//! and export JSON Schema / templates that match load-time checks.

#![cfg(feature = "derive")]

use std::collections::BTreeMap;
use std::fs;

use clapfig::runtime::Shape;
use clapfig::{Clapfig, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
struct RustParams {
    shape: String,
}

#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
struct PayloadParams {
    artifact: String,
    entry: String,
}

#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind")]
enum Block {
    #[serde(rename = "rust")]
    Rust {
        mount: String,
        #[clapfig(optional)]
        crate_path: Option<String>,
        params: RustParams,
    },
    #[serde(rename = "payload")]
    Payload {
        mount: String,
        params: PayloadParams,
    },
    #[serde(rename = "off")]
    Off,
}

#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
struct App {
    block: Block,
}

#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Sites {
    blocks: BTreeMap<String, Block>,
}

fn write_and_load_typed<C: clapfig::DocumentRoot + serde::de::DeserializeOwned>(
    contents: &str,
) -> Result<C, clapfig::ClapfigError> {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("app.toml"), contents).unwrap();
    Clapfig::typed::<C>()
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
}

#[test]
fn tagged_enum_two_variant_structs_load_and_typed_deserialize() {
    let cfg: Block = write_and_load_typed(
        "kind = \"rust\"\nmount = \".\"\n[params]\nshape = \"cli-plus-lib\"\n",
    )
    .unwrap();
    assert_eq!(
        cfg,
        Block::Rust {
            mount: ".".into(),
            crate_path: None,
            params: RustParams {
                shape: "cli-plus-lib".into(),
            },
        }
    );
}

#[test]
fn serde_deserialize_of_the_same_tree_succeeds() {
    let table = {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("app.toml"),
            "kind = \"payload\"\nmount = \"/data\"\n[params]\nartifact = \"out\"\nentry = \"start\"\n",
        )
        .unwrap();
        Clapfig::builder(Block::shape())
            .app_name("app")
            .file_name("app.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap()
    };
    let via_serde: Block = clapfig::value::from_value(clapfig::value::Value::Map(table)).unwrap();
    assert_eq!(
        via_serde,
        Block::Payload {
            mount: "/data".into(),
            params: PayloadParams {
                artifact: "out".into(),
                entry: "start".into(),
            },
        }
    );
}

#[test]
fn unit_variant_loads_as_tag_only() {
    let cfg: Block = write_and_load_typed("kind = \"off\"\n").unwrap();
    assert_eq!(cfg, Block::Off);
}

#[test]
fn nested_tagged_field_loads() {
    let cfg: App = write_and_load_typed(
        "[block]\nkind = \"rust\"\nmount = \".\"\n[block.params]\nshape = \"cli\"\n",
    )
    .unwrap();
    match cfg.block {
        Block::Rust { mount, params, .. } => {
            assert_eq!(mount, ".");
            assert_eq!(params.shape, "cli");
        }
        other => panic!("expected rust, got {other:?}"),
    }
}

#[test]
fn map_of_tagged_loads() {
    let cfg: Sites = write_and_load_typed(
        "[blocks.core]\nkind = \"rust\"\nmount = \".\"\n[blocks.core.params]\nshape = \"cli\"\n",
    )
    .unwrap();
    assert_eq!(cfg.blocks.len(), 1);
    match &cfg.blocks["core"] {
        Block::Rust { mount, .. } => assert_eq!(mount, "."),
        other => panic!("expected rust, got {other:?}"),
    }
}

#[test]
fn tagged_json_schema_is_oneof_with_const_no_openapi_discriminator() {
    let s = clapfig::json_schema::generate_from_shape(&Block::shape());
    assert!(
        s.get("discriminator").is_none(),
        "OpenAPI discriminator is not JSON Schema: {s}"
    );
    let one_of = s["oneOf"].as_array().expect("oneOf");
    assert_eq!(one_of.len(), 3);
    let rust = &one_of[0];
    assert_eq!(rust["properties"]["kind"]["const"], "rust");
    assert_eq!(rust["properties"]["kind"]["type"], "string");
    assert_eq!(rust["additionalProperties"], false);
    assert_eq!(rust["patternProperties"]["^//"], serde_json::json!({}));
    assert!(rust["properties"].get("params").is_some(), "{rust}");
    assert_eq!(one_of[1]["properties"]["kind"]["const"], "payload");
    assert_eq!(one_of[2]["properties"]["kind"]["const"], "off");
}

#[test]
fn nested_and_map_of_tagged_json_schema_compose() {
    let nested = clapfig::json_schema::generate_schema(App::schema());
    assert_eq!(nested["patternProperties"]["^//"], serde_json::json!({}));
    assert_eq!(nested["additionalProperties"], false);
    let block = &nested["properties"]["block"];
    assert!(block.get("discriminator").is_none(), "{block}");
    assert_eq!(block["oneOf"][0]["properties"]["kind"]["const"], "rust");

    let mapped = clapfig::json_schema::generate_schema(Sites::schema());
    let entry = &mapped["properties"]["blocks"]["additionalProperties"];
    assert!(entry.get("discriminator").is_none(), "{entry}");
    assert_eq!(entry["oneOf"][0]["properties"]["kind"]["const"], "rust");
    assert_eq!(
        mapped["properties"]["blocks"]["patternProperties"]["^//"],
        serde_json::json!({})
    );
}

#[test]
fn tagged_config_gen_one_commented_example_per_variant() {
    use clapfig::format::{FormatAdapter, TomlAdapter};

    let text = TomlAdapter.template(&Block::shape()).unwrap();
    assert!(
        text.contains("#kind = \"rust\""),
        "rust example must be commented: {text}"
    );
    assert!(
        text.contains("#kind = \"payload\""),
        "payload example must be commented: {text}"
    );
    assert!(
        text.contains("#kind = \"off\""),
        "off example must be commented: {text}"
    );
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("kind") {
            panic!("uncommented mixed-variant object is illegal: {text}");
        }
        if !trimmed.starts_with('#') && trimmed.contains('=') {
            panic!("tagged gen must not emit uncommented assignments: {text}");
        }
    }
}

#[test]
fn object_root_without_tagged_stays_byte_identical() {
    use clapfig::format::{FormatAdapter, TomlAdapter};
    use clapfig::runtime::{Field, Schema as RtSchema};

    let schema = RtSchema::object("App")
        .doc("Demo runtime schema")
        .field("host", Field::string().doc("App host").default("localhost"))
        .field("port", Field::integer().doc("Port number").default(8080i64))
        .build();
    let text = TomlAdapter.template(&Shape::Object(schema)).unwrap();
    assert!(
        text.contains("host = \"localhost\""),
        "object-root defaults stay uncommented: {text}"
    );
    assert!(
        !text.contains("kind ="),
        "object-root must not invent a tagged example: {text}"
    );
}

#[test]
fn derive_shape_is_tagged_with_closed_set() {
    match Block::shape() {
        Shape::Tagged(tagged) => {
            assert_eq!(tagged.tag, "kind");
            assert_eq!(tagged.name, "Block");
            let names: Vec<&str> = tagged
                .variants
                .iter()
                .map(|v| v.discriminator.as_str())
                .collect();
            assert_eq!(names, ["rust", "payload", "off"]);
            assert!(
                tagged.variants[0]
                    .schema
                    .fields
                    .iter()
                    .any(|f| f.name == "mount")
            );
            assert!(
                !tagged.variants[0]
                    .schema
                    .fields
                    .iter()
                    .any(|f| f.name == "kind"),
                "tag is not a variant field"
            );
            assert!(tagged.variants[2].schema.fields.is_empty());
        }
        other => panic!("expected Tagged, got {other:?}"),
    }
}
