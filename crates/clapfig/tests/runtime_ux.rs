//! End-to-end coverage for the runtime UX contract on the typed
//! (derive) path: value coercions and error categorization during load,
//! and JSON Schema export that matches what the runtime actually
//! accepts.
//!
//! The per-module unit tests pin each mechanism in isolation; these
//! tests pin the user-observable behavior through `Clapfig::typed`:
//!
//! - an integer literal loads into an `f64` field;
//! - an out-of-range integer for a sized width fails naming the key
//!   path, never the opaque `<merged>` placeholder;
//! - a deserialize failure inside a typed `post_validate` hook surfaces
//!   as the type error it is, not as a hook rejection — on `load` and
//!   on merged `config get`/`list` via `handle`;
//! - the exported JSON Schema's `required` arrays list exactly the
//!   absences the runtime rejects, and its leaf schemas carry integer
//!   bounds and the four-form datetime `anyOf`.

#![cfg(feature = "derive")]

use clapfig::{Clapfig, ClapfigError, ConfigAction, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use std::fs;
use tempfile::TempDir;

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ServerCfg {
    /// Seconds before giving up.
    #[clapfig(default = 1.5)]
    timeout: f64,
    /// Retry budget.
    #[clapfig(default = 3)]
    retries: u8,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AppCfg {
    /// Deployment name — no default, must be configured.
    name: String,
    server: ServerCfg,
}

fn load_from(dir: &TempDir) -> Result<AppCfg, ClapfigError> {
    Clapfig::typed::<AppCfg>()
        .app_name("uxdemo")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
}

#[test]
fn integer_literal_loads_into_float_field() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("uxdemo.toml"),
        "name = \"prod\"\n[server]\ntimeout = 5\n",
    )
    .unwrap();
    let cfg = load_from(&dir).expect("serde accepts integers for f64; clapfig must too");
    assert_eq!(cfg.server.timeout, 5.0);
}

#[test]
fn out_of_range_integer_fails_naming_the_key_path() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("uxdemo.toml"),
        "name = \"prod\"\n[server]\nretries = 300\n",
    )
    .unwrap();
    let err = load_from(&dir).unwrap_err();
    match err {
        ClapfigError::InvalidValue { key, reason, .. } => {
            assert_eq!(key, "server.retries", "must name the key, not <merged>");
            assert!(reason.contains("out of range"), "{reason}");
            assert!(reason.contains("0..=255"), "{reason}");
        }
        other => panic!("expected InvalidValue, got {other:?}"),
    }
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct HookCfg {
    /// Free-form at the schema layer, but the Rust type wants a string.
    #[clapfig(value)]
    rule: String,
}

#[test]
fn post_validate_deserialize_failure_is_a_type_error_not_a_hook_rejection() {
    let dir = TempDir::new().unwrap();
    // Schema-level check passes (`value` accepts any shape); the typed
    // hook's Map → HookCfg deserialize is what fails.
    fs::write(dir.path().join("uxdemo.toml"), "rule = 5\n").unwrap();
    let err = Clapfig::typed::<HookCfg>()
        .app_name("uxdemo")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .post_validate(|_cfg| Ok(()))
        .load()
        .unwrap_err();
    match &err {
        ClapfigError::InvalidValue { .. } => {}
        other => panic!("expected InvalidValue, got {other:?}"),
    }
    assert!(
        !err.to_string().contains("Configuration validation failed"),
        "must not wear the post_validate rejection prefix: {err}"
    );
}

#[test]
fn handle_get_list_deserialize_failure_is_a_type_error_not_a_hook_rejection() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("uxdemo.toml"), "rule = 5\n").unwrap();
    let builder = || {
        Clapfig::typed::<HookCfg>()
            .app_name("uxdemo")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .post_validate(|_cfg| Ok(()))
    };
    for action in [
        ConfigAction::Get {
            key: "rule".into(),
            scope: None,
        },
        ConfigAction::List { scope: None },
    ] {
        let err = builder().handle(&action).unwrap_err();
        match &err {
            ClapfigError::InvalidValue { .. } => {}
            other => panic!("expected InvalidValue for {action:?}, got {other:?}"),
        }
        assert!(
            !err.to_string().contains("Configuration validation failed"),
            "must not wear the post_validate rejection prefix: {err}"
        );
    }
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct RoundTripCfg {
    name: String,
    #[clapfig(default = "localhost")]
    host: String,
    auth: AuthCfg,
    limits: LimitsCfg,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AuthCfg {
    token: String,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct LimitsCfg {
    #[clapfig(default = 10)]
    max: u16,
}

/// Every dotted path listed in a schema object's `required` arrays,
/// recursing through `properties`.
fn collect_required(schema: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for name in required {
            let name = name.as_str().unwrap();
            out.push(if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}.{name}")
            });
        }
    }
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (name, child) in props {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            collect_required(child, &path, out);
        }
    }
}

#[test]
fn exported_required_matches_what_the_runtime_rejects_when_absent() {
    let schema = clapfig::json_schema::generate_schema(RoundTripCfg::shape());

    // The exported schema requires exactly the defaultless leaves and
    // the sections transitively containing them.
    let mut required = Vec::new();
    collect_required(&schema, "", &mut required);
    required.sort();
    assert_eq!(required, ["auth", "auth.token", "name"]);

    // Positive direction: a document supplying only those keys loads —
    // every non-required absence is satisfied by synthesized defaults,
    // so an external validator enforcing this schema accepts what
    // clapfig accepts.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("rt.toml"),
        "name = \"x\"\n[auth]\ntoken = \"t\"\n",
    )
    .unwrap();
    let cfg: RoundTripCfg = Clapfig::typed::<RoundTripCfg>()
        .app_name("rt")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.host, "localhost");
    assert_eq!(cfg.limits.max, 10);

    // Negative direction: dropping a required key fails the load, so
    // the schema isn't looser than the runtime either.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("rt.toml"), "name = \"x\"\n").unwrap();
    let err = Clapfig::typed::<RoundTripCfg>()
        .app_name("rt")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap_err();
    match err {
        ClapfigError::MissingRequired { key, .. } => assert_eq!(key, "auth.token"),
        other => panic!("expected MissingRequired, got {other:?}"),
    }
}

#[test]
fn exported_schema_carries_integer_bounds_and_datetime_format() {
    #[derive(Schema, Serialize, Deserialize, Debug)]
    struct MetaCfg {
        retries: u8,
        #[clapfig(default = "1979-05-27T07:32:00Z")]
        starts_at: clapfig::value::Datetime,
    }
    let schema = clapfig::json_schema::generate_schema(MetaCfg::shape());
    let props = &schema["properties"];
    assert_eq!(props["retries"]["type"], "integer");
    assert_eq!(props["retries"]["minimum"], 0);
    assert_eq!(props["retries"]["maximum"], 255);
    assert_eq!(props["starts_at"]["type"], "string");
    // `format: "date-time"` is only the offset form; it rides one
    // anyOf branch. The rest are range-aware patterns for TOML's
    // four lexical forms (RFC 3339 offset, TOML offset variants,
    // local date-time, local date, local time).
    assert!(props["starts_at"].get("format").is_none());
    assert!(props["starts_at"]["anyOf"].is_array());
    assert_eq!(props["starts_at"]["anyOf"].as_array().unwrap().len(), 5);
    assert_eq!(props["starts_at"]["default"], "1979-05-27T07:32:00Z");
}
