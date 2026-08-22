//! The artifact pair and its editor schema directive (#103).
//!
//! Covers what a consumer generating an editor-discoverable config file
//! depends on: the directive is the template's first line with one blank
//! line under it, the two artifacts come from one schema, both schema
//! entry points (runtime `Shape` and `#[derive(Schema)]`) answer the same
//! way, a template carrying the directive still loads as config, and
//! omitting the reference leaves `config gen` / `config schema` output
//! untouched byte for byte.

use std::collections::BTreeMap;

use clapfig::artifacts::{ArtifactOptions, SchemaReference};
use clapfig::error::ClapfigError;
use clapfig::format::{Operation, UnsupportedByFormat};
use clapfig::runtime::{Field, Schema as RtSchema};
use clapfig::{Clapfig, ConfigAction, ConfigResult, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const REFERENCE: &str = "./blocks.schema.json";

/// The runtime twin of [`BlocksFile`] below: a named map of block
/// instances, each with a required leaf and a defaulted one.
///
/// The derive path spells one doc comment on a map field and it reaches
/// both the field and the entry schema, so the runtime twin sets the same
/// text on both to describe the same thing.
fn blocks_schema() -> RtSchema {
    RtSchema::object("BlocksFile")
        .doc("The repo's block instances.")
        .field(
            "block",
            Field::map_of(
                RtSchema::object("BlockDecl")
                    .doc("One table per instance: `[block.<name>]`.")
                    .field("kind", Field::string().doc("The kind this instance is of."))
                    .field(
                        "mount",
                        Field::string()
                            .doc("Repo-relative directory the block sits at.")
                            .default("."),
                    ),
            )
            .doc("One table per instance: `[block.<name>]`."),
        )
        .build()
}

/// The repo's block instances.
#[derive(clapfig::Schema, Serialize, Deserialize, Debug, Default)]
struct BlocksFile {
    /// One table per instance: `[block.<name>]`.
    block: BTreeMap<String, BlockDecl>,
}

#[derive(clapfig::Schema, Serialize, Deserialize, Debug)]
struct BlockDecl {
    /// The kind this instance is of.
    kind: String,

    /// Repo-relative directory the block sits at.
    #[clapfig(default = ".")]
    mount: String,
}

fn reference() -> SchemaReference {
    SchemaReference::new(REFERENCE).unwrap()
}

fn with_reference() -> ArtifactOptions {
    ArtifactOptions::new().schema_reference(reference())
}

fn runtime_builder() -> clapfig::Builder {
    Clapfig::builder(blocks_schema()).app_name("edward")
}

fn template_of(result: ConfigResult) -> String {
    match result {
        ConfigResult::Template(text) => text,
        other => panic!("expected a template, got {other:?}"),
    }
}

fn schema_of(result: ConfigResult) -> String {
    match result {
        ConfigResult::Schema(text) => text,
        other => panic!("expected a schema, got {other:?}"),
    }
}

// --- directive placement ------------------------------------------------

#[test]
fn directive_is_the_first_line_followed_by_a_blank_separator() {
    let pair = runtime_builder().artifacts(&with_reference()).unwrap();
    let mut lines = pair.template.lines();
    assert_eq!(lines.next(), Some("#:schema ./blocks.schema.json"));
    assert_eq!(lines.next(), Some(""));
    // The third line is where the un-prefixed template body starts.
    let body = template_of(
        runtime_builder()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap(),
    );
    assert_eq!(lines.next(), body.lines().next());
}

#[test]
fn directive_only_prefixes_the_gen_template() {
    // The body under the directive is `config gen` output, unchanged —
    // the directive is added, nothing else is rewritten.
    let pair = runtime_builder().artifacts(&with_reference()).unwrap();
    let body = template_of(
        runtime_builder()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap(),
    );
    assert_eq!(
        pair.template,
        format!("#:schema {REFERENCE}\n\n{body}"),
        "template:\n{}",
        pair.template
    );
}

#[test]
fn the_reference_goes_in_verbatim() {
    // Clapfig does not resolve, rewrite, or normalize the reference: a
    // URL, an absolute path, and a bare file name each land as written.
    for text in [
        "https://example.com/schemas/blocks.v1.json",
        "/etc/edward/blocks.schema.json",
        "blocks.schema.json",
        "../shared/schema with spaces.json",
    ] {
        let options = ArtifactOptions::new().schema_reference(SchemaReference::new(text).unwrap());
        let pair = runtime_builder().artifacts(&options).unwrap();
        assert_eq!(
            pair.template.lines().next().unwrap(),
            format!("#:schema {text}")
        );
    }
}

#[test]
fn a_template_with_a_directive_is_still_loadable_config() {
    // The directive is a TOML comment, so a generated file carrying it
    // loads to the same configuration as one without it — including
    // under default strict mode, which would reject a stray key.
    let dir = TempDir::new().unwrap();
    let pair = runtime_builder().artifacts(&with_reference()).unwrap();
    let path = dir.path().join("edward.toml");
    std::fs::write(
        &path,
        format!("{}\n[block.core]\nkind = \"rust\"\n", pair.template),
    )
    .unwrap();

    let loaded = Clapfig::builder(blocks_schema())
        .app_name("edward")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();

    let block = loaded["block"].as_map().unwrap();
    assert_eq!(
        block["core"].as_map().unwrap()["kind"].as_str(),
        Some("rust")
    );
}

#[test]
fn kebab_normalization_reaches_the_body_not_the_directive() {
    let schema = RtSchema::object("App")
        .field("pool_size", Field::integer().default(5i64))
        .build();
    let pair = Clapfig::builder(schema)
        .app_name("app")
        .normalize_keys(true)
        .artifacts(&with_reference())
        .unwrap();
    assert_eq!(
        pair.template.lines().next().unwrap(),
        format!("#:schema {REFERENCE}")
    );
    assert!(pair.template.contains("pool-size"), "{}", pair.template);
}

// --- opt-out compatibility ---------------------------------------------

#[test]
fn without_a_reference_the_artifacts_match_the_standalone_actions() {
    let pair = runtime_builder()
        .artifacts(&ArtifactOptions::new())
        .unwrap();
    let generated = template_of(
        runtime_builder()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap(),
    );
    let schema = schema_of(
        runtime_builder()
            .handle(&ConfigAction::Schema { output: None })
            .unwrap(),
    );
    assert_eq!(pair.template, generated);
    assert_eq!(pair.schema, schema);
}

#[test]
fn a_reference_does_not_touch_the_json_schema() {
    // Only the template carries the binding; the schema document is the
    // same either way (it does not learn where it will be written).
    let plain = runtime_builder()
        .artifacts(&ArtifactOptions::new())
        .unwrap();
    let bound = runtime_builder().artifacts(&with_reference()).unwrap();
    assert_eq!(plain.schema, bound.schema);
}

// --- both schema entry points ------------------------------------------

#[test]
fn derive_and_runtime_paths_generate_the_same_pair() {
    let derived = Clapfig::typed::<BlocksFile>()
        .app_name("edward")
        .artifacts(&with_reference())
        .unwrap();
    let runtime = runtime_builder().artifacts(&with_reference()).unwrap();
    assert_eq!(derived.template, runtime.template);
    assert_eq!(derived.schema, runtime.schema);
}

#[test]
fn derive_path_without_a_reference_matches_its_standalone_actions() {
    let pair = Clapfig::typed::<BlocksFile>()
        .app_name("edward")
        .artifacts(&ArtifactOptions::new())
        .unwrap();
    let generated = template_of(
        Clapfig::typed::<BlocksFile>()
            .app_name("edward")
            .handle(&ConfigAction::Gen { output: None })
            .unwrap(),
    );
    let schema = schema_of(
        Clapfig::typed::<BlocksFile>()
            .app_name("edward")
            .handle(&ConfigAction::Schema { output: None })
            .unwrap(),
    );
    assert_eq!(pair.template, generated);
    assert_eq!(pair.schema, schema);
}

// --- reference validation ----------------------------------------------

#[test]
fn a_multi_line_reference_is_rejected_before_anything_is_generated() {
    let err = SchemaReference::new("./blocks.schema.json\nport = 1").unwrap_err();
    assert!(
        matches!(err, ClapfigError::InvalidSchemaReference { .. }),
        "{err:?}"
    );
}

// --- format capability --------------------------------------------------

#[test]
fn formats_without_a_directive_refuse_instead_of_dropping_the_reference() {
    for format in ["yaml", "json"] {
        let err = Clapfig::builder(blocks_schema())
            .app_name("edward")
            .file_stem("edward")
            .formats([format])
            .artifacts(&with_reference())
            .unwrap_err();
        match err {
            ClapfigError::Format(inner) => {
                let refusal: UnsupportedByFormat = match inner {
                    clapfig::format::FormatError::Unsupported(u) => u,
                    other => panic!("expected a capability refusal, got {other:?}"),
                };
                assert_eq!(refusal.operation, Operation::SchemaDirective);
                assert_eq!(refusal.format, format);
            }
            other => panic!("expected a format error, got {other:?}"),
        }
    }
}

#[test]
fn formats_without_a_directive_still_generate_the_pair_without_one() {
    let pair = Clapfig::builder(blocks_schema())
        .app_name("edward")
        .file_stem("edward")
        .formats(["yaml"])
        .artifacts(&ArtifactOptions::new())
        .unwrap();
    assert!(!pair.template.contains("#:schema"), "{}", pair.template);
    assert!(pair.schema.contains("\"BlocksFile\""), "{}", pair.schema);
}

#[test]
fn the_template_is_rendered_in_the_preferred_format() {
    // Artifacts follow the same rule stdout `config gen` does: the first
    // enabled format renders. The directive rides that choice.
    let pair = Clapfig::builder(blocks_schema())
        .app_name("edward")
        .file_stem("edward")
        .formats(["toml", "yaml"])
        .artifacts(&with_reference())
        .unwrap();
    assert_eq!(
        pair.template.lines().next().unwrap(),
        format!("#:schema {REFERENCE}")
    );
}

#[test]
fn generating_artifacts_needs_an_app_name() {
    let err = Clapfig::builder(blocks_schema())
        .artifacts(&with_reference())
        .unwrap_err();
    assert!(matches!(err, ClapfigError::AppNameRequired), "{err:?}");
}
