//! The artifact pair and its editor schema directive (#103).
//!
//! Covers what a consumer generating an editor-discoverable config file
//! depends on: the directive is the template's first line with one blank
//! line under it, the two artifacts come from one schema, both schema
//! entry points (runtime `Shape` and `#[derive(Schema)]`) answer the same
//! way, a template carrying the directive still loads as config, and
//! omitting the reference leaves `config gen` / `config schema` output
//! untouched byte for byte. Under `normalize_keys(true)` the template
//! writes kebab-case keys and the schema accepts both that spelling and
//! the declared snake_case one, since that builder loads either — an
//! editor validating against a schema naming only one of them would flag
//! a file clapfig reads without complaint.

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

/// A schema shaped to put a multiword key everywhere the renaming has to
/// reach: the document root, a nested section, a map entry, and a tagged
/// union's tag and variant fields.
fn multiword_schema() -> RtSchema {
    RtSchema::object("App")
        .field("pool_size", Field::integer().default(5i64))
        .nested(
            "db_pool",
            RtSchema::object("DbPool").field("max_idle", Field::integer().default(2i64)),
        )
        .map_of(
            "named_blocks",
            RtSchema::object("Block").field("mount_point", Field::string().default(".")),
        )
        .field(
            "step_kind",
            clapfig::runtime::Shape::tagged("StepKind", "step_type")
                .variant(
                    "shell",
                    RtSchema::object("Shell")
                        .field("run_line", Field::string().default("true"))
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Every property name the schema document declares, at any depth —
/// enough to assert on spelling without pinning the document's exact
/// structure.
fn property_names(value: &serde_json::Value, out: &mut Vec<String>) {
    collect_member(value, "properties", &mut |child| {
        if let Some(properties) = child.as_object() {
            out.extend(properties.keys().cloned());
        }
    });
}

fn collect_member(
    value: &serde_json::Value,
    member: &str,
    take: &mut impl FnMut(&serde_json::Value),
) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name == member {
                    take(child);
                }
                collect_member(child, member, take);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_member(item, member, take);
            }
        }
        _ => {}
    }
}

/// The alias rules an object schema carries, in declaration order.
fn alias_rules(object: &serde_json::Value) -> Vec<serde_json::Value> {
    object
        .get("allOf")
        .and_then(|rules| rules.as_array())
        .cloned()
        .unwrap_or_default()
}

/// The schema document a `normalize_keys(true)` builder over `schema`
/// generates, parsed.
fn normalized_document(schema: RtSchema) -> serde_json::Value {
    let pair = Clapfig::builder(schema)
        .app_name("app")
        .normalize_keys(true)
        .artifacts(&with_reference())
        .unwrap();
    serde_json::from_str(&pair.schema).unwrap()
}

#[test]
fn the_schema_names_both_spellings_of_every_key_under_normalization() {
    // The pair's promise is that the schema describes the documents the
    // builder beside it loads. That builder normalizes, so it loads a
    // multiword key written either way, and object schemas are closed
    // (`additionalProperties: false`) — naming only one spelling would
    // make an editor reject a file clapfig reads. The kebab half is what
    // the generated template writes; the snake half is what the schema
    // declares and what a user who never saw the template writes.
    let pair = Clapfig::builder(multiword_schema())
        .app_name("app")
        .normalize_keys(true)
        .artifacts(&with_reference())
        .unwrap();

    let document: serde_json::Value = serde_json::from_str(&pair.schema).unwrap();
    let mut declared = Vec::new();
    property_names(&document, &mut declared);

    for (written, alias) in [
        ("pool-size", "pool_size"),
        ("db-pool", "db_pool"),
        ("max-idle", "max_idle"),
        ("named-blocks", "named_blocks"),
        ("mount-point", "mount_point"),
        ("step-kind", "step_kind"),
        ("step-type", "step_type"),
        ("run-line", "run_line"),
    ] {
        assert!(declared.contains(&written.to_string()), "{declared:?}");
        assert!(declared.contains(&alias.to_string()), "{declared:?}");
    }

    // The template still writes exactly one spelling: the kebab one.
    for (written, alias) in [
        ("pool-size", "pool_size"),
        ("db-pool", "db_pool"),
        ("max-idle", "max_idle"),
        ("mount-point", "mount_point"),
    ] {
        assert!(pair.template.contains(written), "{}", pair.template);
        assert!(!pair.template.contains(alias), "{}", pair.template);
    }
}

#[test]
fn a_multiword_key_carries_the_same_subschema_under_both_spellings() {
    // Two names for one field, so an editor gives the same type, doc, and
    // default whichever the user typed.
    let document = normalized_document(multiword_schema());
    let properties = &document["properties"];
    assert_eq!(properties["pool-size"], properties["pool_size"]);
    assert_eq!(properties["db-pool"], properties["db_pool"]);
    assert!(properties["pool-size"].is_object());
}

#[test]
fn a_required_multiword_key_is_satisfied_by_either_spelling() {
    // `required: ["api-key"]` would reject the snake spelling and
    // `required: ["api-key", "api_key"]` would demand both, which the
    // load path refuses. `oneOf` over the two says what the runtime
    // means: exactly one of them.
    let schema = RtSchema::object("App")
        .field("api_key", Field::string())
        .build();
    let document = normalized_document(schema);

    // The object requires the key through its alias rule, so it carries
    // no `required` array of its own to name one spelling in.
    assert!(document.get("required").is_none(), "{document}");
    assert_eq!(
        alias_rules(&document),
        vec![serde_json::json!({
            "oneOf": [{ "required": ["api-key"] }, { "required": ["api_key"] }]
        })]
    );
}

#[test]
fn an_optional_multiword_key_may_be_written_under_at_most_one_spelling() {
    // `pool_size` has a default, so absence is fine — but a table holding
    // both spellings is the collision the load path refuses rather than
    // picking a winner by key order, and the schema refuses it too.
    let schema = RtSchema::object("App")
        .field("pool_size", Field::integer().default(5i64))
        .build();
    let document = normalized_document(schema);

    assert_eq!(
        alias_rules(&document),
        vec![serde_json::json!({
            "not": { "required": ["pool-size", "pool_size"] }
        })]
    );
}

#[test]
fn a_single_word_key_gets_one_name_and_a_plain_required_entry() {
    // Nothing to alias: normalization only ever rewrites `_`, so these
    // keys are spelled the one way in both artifacts and stay in
    // `required`, where a validator's message names the missing key.
    let schema = RtSchema::object("App")
        .field("host", Field::string())
        .build();
    let document = normalized_document(schema);

    let mut declared = Vec::new();
    property_names(&document, &mut declared);
    assert_eq!(declared, vec!["host".to_string()]);
    assert_eq!(document["required"], serde_json::json!(["host"]));
    assert!(alias_rules(&document).is_empty());
}

#[test]
fn a_multiword_union_tag_is_required_under_either_spelling() {
    // The tag is a key like any other. The discriminator is a value and
    // keeps its declared spelling on both branches.
    let document = normalized_document(multiword_schema());
    let branch = &document["properties"]["step-kind"]["oneOf"][0];

    assert_eq!(
        branch["properties"]["step-type"],
        serde_json::json!({ "type": "string", "const": "shell" })
    );
    assert_eq!(
        branch["properties"]["step_type"],
        branch["properties"]["step-type"]
    );
    assert!(branch.get("required").is_none(), "{branch}");
    assert_eq!(
        alias_rules(branch).first().unwrap(),
        &serde_json::json!({
            "oneOf": [{ "required": ["step-type"] }, { "required": ["step_type"] }]
        })
    );
}

#[test]
fn normalization_leaves_discriminator_values_alone() {
    // Only key spellings are renamed. A discriminator is a value, so
    // `two_step` stays as declared in both artifacts.
    let schema = RtSchema::object("App")
        .field(
            "step_kind",
            clapfig::runtime::Shape::tagged("StepKind", "step_type")
                .variant(
                    "two_step",
                    RtSchema::object("TwoStep")
                        .field("run_line", Field::string().default("true"))
                        .build(),
                )
                .build(),
        )
        .build();
    let pair = Clapfig::builder(schema)
        .app_name("app")
        .normalize_keys(true)
        .artifacts(&ArtifactOptions::new())
        .unwrap();
    assert!(pair.schema.contains("two_step"), "{}", pair.schema);
    assert!(!pair.schema.contains("two-step"), "{}", pair.schema);
}

#[test]
fn the_standalone_schema_action_normalizes_the_same_way() {
    // `config schema` and the pair must not describe the config file
    // differently — the pair's schema IS that action's output.
    let action = schema_of(
        Clapfig::builder(multiword_schema())
            .app_name("app")
            .normalize_keys(true)
            .handle(&ConfigAction::Schema { output: None })
            .unwrap(),
    );
    let pair = Clapfig::builder(multiword_schema())
        .app_name("app")
        .normalize_keys(true)
        .artifacts(&with_reference())
        .unwrap();
    assert_eq!(pair.schema, action);
}

#[test]
fn without_normalization_the_declared_spelling_is_untouched() {
    // The renaming is opt-in: default builders describe and render the
    // schema's own snake_case keys.
    let pair = Clapfig::builder(multiword_schema())
        .app_name("app")
        .artifacts(&with_reference())
        .unwrap();
    assert!(pair.schema.contains("pool_size"), "{}", pair.schema);
    assert!(!pair.schema.contains("pool-size"), "{}", pair.schema);
    assert!(pair.template.contains("pool_size"), "{}", pair.template);

    // No second spelling to reconcile, so no alias rules either.
    let document: serde_json::Value = serde_json::from_str(&pair.schema).unwrap();
    let mut rules = Vec::new();
    collect_member(&document, "allOf", &mut |child| rules.push(child.clone()));
    assert!(rules.is_empty(), "{rules:?}");
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
    // With no file naming configured, the preferred format comes from the
    // default `<app>.toml` naming — which needs the app name.
    let err = Clapfig::builder(blocks_schema())
        .artifacts(&with_reference())
        .unwrap_err();
    assert!(matches!(err, ClapfigError::AppNameRequired), "{err:?}");
}

#[test]
fn explicit_file_naming_generates_artifacts_without_an_app_name() {
    // The other half of that error contract: either naming call resolves
    // the preferred format on its own, so `AppNameRequired` is not a
    // blanket requirement of `artifacts()`.
    for builder in [
        Clapfig::builder(blocks_schema()).file_name("blocks.toml"),
        Clapfig::builder(blocks_schema()).file_stem("blocks"),
    ] {
        let pair = builder.artifacts(&with_reference()).unwrap();
        assert_eq!(
            pair.template.lines().next().unwrap(),
            format!("#:schema {REFERENCE}")
        );
        assert!(pair.schema.contains("\"BlocksFile\""), "{}", pair.schema);
    }
}

// --- error rendering ----------------------------------------------------

#[test]
fn a_rejected_reference_is_escaped_in_the_error_message() {
    // The value was rejected *for* carrying control characters, so
    // replaying them raw would let a rejected reference forge log lines
    // or emit terminal escapes. The message quotes it through `Debug`.
    let newline = SchemaReference::new("./a.json\nport = 1")
        .unwrap_err()
        .to_string();
    assert!(newline.contains("\\n"), "{newline}");
    assert!(!newline.contains('\n'), "{newline}");

    let escape = SchemaReference::new("./a\u{1b}[31m.json")
        .unwrap_err()
        .to_string();
    assert!(escape.contains("\\u{1b}"), "{escape}");
    assert!(!escape.contains('\u{1b}'), "{escape}");

    // The raw value stays available to callers inspecting the error.
    match SchemaReference::new("./a.json\nport = 1").unwrap_err() {
        ClapfigError::InvalidSchemaReference { reference, .. } => {
            assert_eq!(reference, "./a.json\nport = 1");
        }
        other => panic!("expected InvalidSchemaReference, got {other:?}"),
    }
}
