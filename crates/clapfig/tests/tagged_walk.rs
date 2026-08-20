//! Runtime two-phase tagged walk (SHP01-WS04 / #168).
//!
//! Internally tagged object-root (and nested tagged fields) load through
//! the existing resolve pipeline. JSON Schema / templates stay WS05 stubs.

use std::fs;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use clapfig::runtime::{Field, Schema, Shape};
use clapfig::types::{InputType, SearchPath};
use clapfig::{Clapfig, ClapfigError, UnknownKeyContext, UnknownKeyDecision, value::Value};
use tempfile::TempDir;

fn block_shape() -> Shape {
    Shape::from(
        Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("mount", Field::string())
                    .field("crate_path", Field::string().optional())
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field("mount", Field::string())
                    .field("artifact", Field::string())
                    .build(),
            )
            .variant("off", Schema::object("Off").build())
            .build(),
    )
}

fn load_file(shape: Shape, contents: &str) -> Result<clapfig::value::Map, ClapfigError> {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("app.toml"), contents).unwrap();
    Clapfig::builder(shape)
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
}

#[test]
fn good_rust_instance_loads() {
    let table = load_file(block_shape(), "kind = \"rust\"\nmount = \".\"\n").unwrap();
    assert_eq!(table["kind"], Value::String("rust".into()));
    assert_eq!(table["mount"], Value::String(".".into()));
}

#[test]
fn unit_variant_loads_as_tag_only() {
    let table = load_file(block_shape(), "kind = \"off\"\n").unwrap();
    assert_eq!(table["kind"], Value::String("off".into()));
    assert_eq!(table.len(), 1);
}

#[test]
fn unknown_discriminator_is_invalid_value_on_tag_with_origin_and_allowed_set() {
    let err = load_file(block_shape(), "kind = \"rus\"\nmount = \".\"\n").unwrap_err();
    match err {
        ClapfigError::InvalidValue {
            key,
            reason,
            origin,
        } => {
            assert_eq!(key, "kind");
            assert!(reason.contains("not in allowed set"), "{reason}");
            assert!(reason.contains("\"rust\""), "{reason}");
            assert!(reason.contains("\"payload\""), "{reason}");
            assert_eq!(origin.input_type, Some(InputType::File));
            assert!(
                origin.file.is_some(),
                "unknown discriminator must name origin"
            );
            assert!(origin.span.is_some());
        }
        other => panic!("expected InvalidValue, got {other:?}"),
    }
}

#[test]
fn missing_tag_is_missing_required_with_discovery() {
    let err = load_file(block_shape(), "mount = \".\"\n").unwrap_err();
    match err {
        ClapfigError::MissingRequired {
            ref key,
            ref discovery,
        } => {
            assert_eq!(key, "kind");
            assert!(
                discovery
                    .files
                    .iter()
                    .any(|p| p.outcome == clapfig::error::ProbeOutcome::Loaded),
                "missing tag carries the search, not an origin: {discovery:?}"
            );
        }
        other => panic!("expected MissingRequired, got {other:?}"),
    }
    assert!(
        !err.to_string().contains("set by"),
        "MissingRequired must not name a winning origin: {err}"
    );
}

#[test]
fn payload_only_key_on_rust_instance_is_unknown_key() {
    let err = load_file(
        block_shape(),
        "kind = \"rust\"\nmount = \".\"\nartifact = \"x\"\n",
    )
    .unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "artifact");
}

#[test]
fn nested_tagged_field_loads() {
    let schema = Schema::object("App").field("block", block_shape()).build();
    let table = load_file(schema.into(), "[block]\nkind = \"rust\"\nmount = \".\"\n").unwrap();
    let block = table["block"].as_map().unwrap();
    assert_eq!(block["kind"], Value::String("rust".into()));
    assert_eq!(block["mount"], Value::String(".".into()));
}

#[test]
fn true_unknown_on_sparse_file_is_rejected_pre_merge() {
    let err = load_file(block_shape(), "not_a_field = 1\n").unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "not_a_field");
}

#[test]
fn sparse_variant_field_without_discriminator_is_legal_at_phase1() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("app.toml"), "mount = \".\"\n").unwrap();
    let prefix = "CLAPFIG_SHP01_WS04_SPARSE";
    let kind = format!("{prefix}__KIND");
    unsafe { std::env::set_var(&kind, "rust") };
    let result = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .env_prefix(prefix)
        .load();
    unsafe { std::env::remove_var(&kind) };
    let table = result.expect("sparse file without tag is legal at phase 1");
    assert_eq!(table["kind"], Value::String("rust".into()));
    assert_eq!(table["mount"], Value::String(".".into()));
}

#[test]
fn kind_change_across_layers_reports_old_kind_keys_at_phase2_with_winner_origins() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\ncrate_path = \"src\"\n",
    )
    .unwrap();
    let prefix = "CLAPFIG_SHP01_WS04_KINDCHANGE";
    let kind = format!("{prefix}__KIND");
    let artifact = format!("{prefix}__ARTIFACT");
    unsafe {
        std::env::set_var(&kind, "payload");
        std::env::set_var(&artifact, "out");
    }
    let result = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .env_prefix(prefix)
        .load();
    unsafe {
        std::env::remove_var(&kind);
        std::env::remove_var(&artifact);
    }
    let err = result.unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "crate_path");
    assert_eq!(keys[0].input_type, Some(InputType::File));
    assert_ne!(keys[0].path.as_os_str(), "<env>");
}

#[test]
fn true_unknown_on_sparse_env_is_rejected_pre_merge() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\n",
    )
    .unwrap();
    let prefix = "CLAPFIG_SHP01_WS04_ENVUNK";
    let rogue = format!("{prefix}__NOT_A_FIELD");
    unsafe { std::env::set_var(&rogue, "1") };
    let result = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .env_prefix(prefix)
        .load();
    unsafe { std::env::remove_var(&rogue) };
    let err = result.unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "not_a_field");
    assert!(keys[0].env_var.as_deref().is_some());
}

#[test]
fn phase1_reject_invokes_callback_once_and_fails() {
    let calls = Arc::new(AtomicUsize::new(0));
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("app.toml"), "not_a_field = 1\n").unwrap();
    let counted = Arc::clone(&calls);
    let result = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .on_unknown_key(move |_c: &UnknownKeyContext<'_>| {
            counted.fetch_add(1, Ordering::SeqCst);
            UnknownKeyDecision::Reject
        })
        .load();
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn phase1_accept_invokes_callback_once_without_collection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\nnot_a_field = 1\n",
    )
    .unwrap();
    let counted = Arc::clone(&calls);
    let (table, collected) = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .on_unknown_key(move |c: &UnknownKeyContext<'_>| {
            counted.fetch_add(1, Ordering::SeqCst);
            if c.leaf == "not_a_field" {
                UnknownKeyDecision::Accept
            } else {
                UnknownKeyDecision::Reject
            }
        })
        .load_with_unknowns()
        .expect("Accept keeps loading");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(collected.is_empty());
    assert_eq!(table["kind"], Value::String("rust".into()));
}

#[test]
fn phase1_collect_invokes_callback_once_and_appends_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\nnot_a_field = 1\n",
    )
    .unwrap();
    let counted = Arc::clone(&calls);
    let (_table, collected) = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .on_unknown_key(move |c: &UnknownKeyContext<'_>| {
            counted.fetch_add(1, Ordering::SeqCst);
            if c.leaf == "not_a_field" {
                UnknownKeyDecision::Collect
            } else {
                UnknownKeyDecision::Reject
            }
        })
        .load_with_unknowns()
        .expect("Collect keeps loading");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].leaf, "not_a_field");
}

#[test]
fn phase1_lenient_invokes_no_callback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\nnot_a_field = 1\n",
    )
    .unwrap();
    let counted = Arc::clone(&calls);
    let _table = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict(false)
        .on_unknown_key(move |_c: &UnknownKeyContext<'_>| {
            counted.fetch_add(1, Ordering::SeqCst);
            UnknownKeyDecision::Reject
        })
        .load()
        .expect("cascade-lenient drops the true unknown");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn surviving_true_unknown_is_not_a_phase2_candidate() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "kind = \"rust\"\nmount = \".\"\nnot_a_field = 1\nartifact = \"x\"\n",
    )
    .unwrap();
    let recorded = Arc::clone(&seen);
    let result = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .on_unknown_key(move |c: &UnknownKeyContext<'_>| {
            recorded.lock().unwrap().push(c.leaf.to_string());
            if c.leaf == "not_a_field" {
                UnknownKeyDecision::Accept
            } else {
                UnknownKeyDecision::Reject
            }
        })
        .load();
    let leaves = seen.lock().unwrap().clone();
    assert_eq!(
        leaves.iter().filter(|l| *l == "not_a_field").count(),
        1,
        "true unknown must not be re-invoked at phase 2: {leaves:?}"
    );
    let err = result.unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "artifact");
}

#[test]
fn phase2_file_unknown_key_span_covers_the_key_token() {
    let err = load_file(
        block_shape(),
        "kind = \"rust\"\nmount = \".\"\nartifact = \"x\"\n",
    )
    .unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys[0].key, "artifact");
    let span = keys[0]
        .span
        .expect("phase-2 file winner must carry key span");
    let src = keys[0].source.as_deref().expect("retained source");
    assert_eq!(
        src.get(span.start..span.end),
        Some("artifact"),
        "caret must cover the key token, not the value"
    );
}

#[test]
fn nested_tagged_cli_overrides_from_matches_tag_and_variant_leaves() {
    #[derive(serde::Serialize)]
    struct Block {
        kind: String,
        mount: String,
    }
    #[derive(serde::Serialize)]
    struct Args {
        block: Block,
    }
    let schema = Schema::object("App").field("block", block_shape()).build();
    let dir = TempDir::new().unwrap();
    let args = Args {
        block: Block {
            kind: "rust".into(),
            mount: ".".into(),
        },
    };
    let table = Clapfig::builder(schema)
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .cli_overrides_from(&args)
        .load()
        .expect("nested tagged leaves must be auto-matched");
    let block = table["block"].as_map().unwrap();
    assert_eq!(block["kind"], Value::String("rust".into()));
    assert_eq!(block["mount"], Value::String(".".into()));
}

#[test]
fn nested_tagged_strict_at_is_a_valid_section() {
    let schema = Schema::object("App").field("block", block_shape()).build();
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("app.toml"),
        "[block]\nkind = \"rust\"\nmount = \".\"\nartifact = \"x\"\n",
    )
    .unwrap();
    let table = Clapfig::builder(schema)
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .strict_at("block", false)
        .load()
        .expect("strict_at on a nested tagged section must be accepted");
    let block = table["block"].as_map().unwrap();
    assert_eq!(block["kind"], Value::String("rust".into()));
}

#[test]
fn cli_won_branch_exclusive_key_names_override_origin() {
    let dir = TempDir::new().unwrap();
    let err = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .cli_override("kind", Some("rust"))
        .cli_override("mount", Some("."))
        .cli_override("artifact", Some("x"))
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "artifact");
    assert_eq!(keys[0].input_type, Some(InputType::Override));
    assert_eq!(keys[0].override_key.as_deref(), Some("artifact"));
    assert_ne!(keys[0].path.as_os_str(), "<env>");
    let msg = err.to_string();
    assert!(
        msg.contains("programmatic override"),
        "Display must not classify an override winner as <env>: {msg}"
    );
    assert!(!msg.contains("<env>"), "{msg}");
}

#[cfg(feature = "url")]
#[test]
fn url_won_branch_exclusive_key_names_url_origin() {
    let dir = TempDir::new().unwrap();
    let err = Clapfig::builder(block_shape())
        .app_name("app")
        .file_name("app.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .url_query("kind=rust&mount=.&artifact=x")
        .load()
        .unwrap_err();
    let keys = err.unknown_keys().expect("UnknownKeys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "artifact");
    assert_eq!(keys[0].input_type, Some(InputType::Url));
    assert_eq!(keys[0].url_key.as_deref(), Some("artifact"));
    assert_ne!(keys[0].path.as_os_str(), "<env>");
    let msg = err.to_string();
    assert!(
        msg.contains("URL query"),
        "Display must not classify a URL winner as <env>: {msg}"
    );
    assert!(!msg.contains("<env>"), "{msg}");
}
