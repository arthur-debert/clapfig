//! End-to-end tests for struct-level `rename_all` in
//! `#[derive(clapfig::Schema)]` (#107 / DER01-WS04).
//!
//! The contract under test: the derived schema applies the same field-name
//! conversion serde's deserialize does, for the full serde rule set, so a
//! `rename_all` struct loads, validates strictly, templates, and emits
//! JSON Schema under the converted names — with explicit renames winning
//! over the rule and the conflict/duplicate diagnostics intact.

#![cfg(feature = "derive")]

use clapfig::{Clapfig, ConfigAction, ConfigResult, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

// -- Serde-exact conversion for every supported rule -----------------------
//
// Each struct derives both `Schema` and serde's `Serialize`/`Deserialize`
// under the same non-directional rule, then asserts:
//   1. the schema's field names equal the hardcoded expected spellings
//      (guards against schema and serde being wrong the same way), and
//   2. serde's own key spellings for the same rule are the same strings —
//      serialize the struct and round-trip it back (a non-directional rule
//      applies to both directions, so the serialized keys are exactly what
//      deserialize expects).
//
// The field set covers the conversion edge cases: multi-word, digits after
// a separator (serde's capitalize-consuming digit behavior: `render_2d` →
// `Render2d`), digits inside runs (`tls_v1_3` → `TlsV13`), and single word.
macro_rules! rename_all_parity {
    ($test:ident, $ty:ident, $rule:literal, $expected:expr) => {
        #[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
        #[serde(rename_all = $rule)]
        struct $ty {
            listen_port: u16,
            tls_v1_3: bool,
            render_2d: String,
            host: String,
        }

        #[test]
        fn $test() {
            let expected: [&str; 4] = $expected;
            let names: Vec<&str> = $ty::schema_static().fields.iter().map(|f| f.name).collect();
            assert_eq!(
                names, expected,
                "schema names must be serde's converted spellings"
            );

            let inst = $ty {
                listen_port: 1,
                tls_v1_3: true,
                render_2d: "x".into(),
                host: "h".into(),
            };
            let json = serde_json::to_value(&inst).unwrap();
            let mut serde_keys: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
            serde_keys.sort();
            let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            want.sort();
            assert_eq!(
                serde_keys, want,
                "serde's own {} spelling disagrees with the schema",
                $rule
            );
            // Round-trip: the spellings the schema advertises are the ones
            // serde's deserialize accepts.
            let back: $ty = serde_json::from_value(json).unwrap();
            assert_eq!(back, inst);
        }
    };
}

rename_all_parity!(
    rule_lowercase_is_identity_on_fields,
    LowerCfg,
    "lowercase",
    ["listen_port", "tls_v1_3", "render_2d", "host"]
);
rename_all_parity!(
    rule_uppercase_keeps_underscores,
    UpperCfg,
    "UPPERCASE",
    ["LISTEN_PORT", "TLS_V1_3", "RENDER_2D", "HOST"]
);
rename_all_parity!(
    rule_pascal_case_consumes_capitalization_on_digits,
    PascalCfg,
    "PascalCase",
    ["ListenPort", "TlsV13", "Render2d", "Host"]
);
rename_all_parity!(
    rule_camel_case_lowers_first_char,
    CamelCfg,
    "camelCase",
    ["listenPort", "tlsV13", "render2d", "host"]
);
rename_all_parity!(
    rule_snake_case_is_identity_on_fields,
    SnakeCfg,
    "snake_case",
    ["listen_port", "tls_v1_3", "render_2d", "host"]
);
rename_all_parity!(
    rule_screaming_snake_case_keeps_underscores,
    ScreamingSnakeCfg,
    "SCREAMING_SNAKE_CASE",
    ["LISTEN_PORT", "TLS_V1_3", "RENDER_2D", "HOST"]
);
rename_all_parity!(
    rule_kebab_case_swaps_separators,
    KebabCfg,
    "kebab-case",
    ["listen-port", "tls-v1-3", "render-2d", "host"]
);
rename_all_parity!(
    rule_screaming_kebab_case,
    ScreamingKebabCfg,
    "SCREAMING-KEBAB-CASE",
    ["LISTEN-PORT", "TLS-V1-3", "RENDER-2D", "HOST"]
);

// -- End-to-end: typed loading, strict validation, template, JSON Schema ---

/// Kebab end-to-end fixture. The nested section checks that the rule does
/// NOT leak into the nested struct (serde semantics: `rename_all` is
/// per-container) while the section key itself converts.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
struct KebabApp {
    /// Port to listen on.
    #[clapfig(default = 8080)]
    listen_port: u16,

    /// Retry budget.
    max_retries: Option<u16>,

    /// Outbound HTTP client settings.
    http_client: HttpClient,
}

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
struct HttpClient {
    /// Per-request timeout in milliseconds.
    #[clapfig(default = 250)]
    timeout_ms: u32,
}

#[test]
fn kebab_schema_loads_kebab_file_typed() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("test.toml"),
        "listen-port = 9090\nmax-retries = 3\n[http-client]\ntimeout_ms = 500\n",
    )
    .unwrap();
    let cfg: KebabApp = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.listen_port, 9090);
    assert_eq!(cfg.max_retries, Some(3));
    assert_eq!(cfg.http_client.timeout_ms, 500);
}

#[test]
fn kebab_schema_defaults_apply_under_converted_names() {
    let dir = TempDir::new().unwrap();
    let cfg: KebabApp = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.listen_port, 8080);
    assert_eq!(cfg.max_retries, None);
    assert_eq!(cfg.http_client.timeout_ms, 250);
}

#[test]
fn kebab_schema_strict_rejects_rust_identifier_spelling() {
    // The schema knows exactly one spelling per field — the converted one.
    // The Rust-identifier spelling is now an unknown key, so strict
    // validation rejects it instead of serde silently missing the field.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "listen_port = 9090\n").unwrap();
    let result: Result<KebabApp, _> = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(result.is_err(), "snake spelling must be an unknown key");
}

#[test]
fn kebab_schema_template_emits_converted_keys() {
    let result = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let t = match result {
        ConfigResult::Template(t) => t,
        other => panic!("expected Template, got {other:?}"),
    };
    assert!(
        t.contains("listen-port"),
        "template must use kebab keys. Got:\n{t}"
    );
    assert!(
        t.contains("max-retries"),
        "template must use kebab keys. Got:\n{t}"
    );
    assert!(
        t.contains("http-client"),
        "section keys convert too. Got:\n{t}"
    );
    assert!(
        !t.contains("listen_port") && !t.contains("max_retries"),
        "Rust spellings must not appear. Got:\n{t}"
    );
    // The rule is per-container: the nested struct has no rename_all, so
    // its own field keeps the Rust spelling.
    assert!(
        t.contains("timeout_ms"),
        "nested field keeps its spelling. Got:\n{t}"
    );
}

#[test]
fn kebab_schema_json_schema_uses_converted_names() {
    let result = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let props = v["properties"].as_object().unwrap();
    assert!(props.contains_key("listen-port"), "got: {props:?}");
    assert!(props.contains_key("max-retries"), "got: {props:?}");
    assert!(
        v["properties"]["http-client"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("timeout_ms"),
        "nested struct keeps its own (un-renamed) spelling"
    );
}

// -- normalize_keys interaction ---------------------------------------------
//
// `normalize_keys(true)` canonicalizes incoming keys by rewriting `-` to
// `_` BEFORE validation — it exists so a snake_case schema can accept
// kebab spellings. A schema whose names are themselves kebab (via
// `rename_all = "kebab-case"`) is the opposite convention: normalization
// rewrites the file's kebab keys away from the schema's spelling, so the
// two features are mutually exclusive. The tests pin that boundary in both
// directions so neither side can silently drift.

#[test]
fn kebab_schema_without_normalize_keys_is_the_supported_pairing() {
    // Belt and suspenders for the boundary: the plain (default) builder is
    // how a kebab-case schema consumes its own template output.
    let result = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .no_env()
        .handle(&ConfigAction::Gen { output: None })
        .unwrap();
    let ConfigResult::Template(t) = result else {
        panic!("expected Template");
    };
    let dir = TempDir::new().unwrap();
    // The template ships commented defaults; uncomment one leaf to prove
    // the emitted spelling round-trips through a strict load.
    let uncommented = t.replace("#listen-port =", "listen-port =");
    std::fs::write(dir.path().join("test.toml"), uncommented).unwrap();
    let cfg: KebabApp = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.listen_port, 8080);
}

#[test]
fn kebab_schema_with_normalize_keys_rejects_kebab_spelling() {
    // With normalization on, the file's `listen-port` becomes
    // `listen_port` before validation — which no longer matches the
    // schema's `listen-port` — so strict validation rejects it. Loudly
    // incompatible, never silently mis-loaded.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "listen-port = 9090\n").unwrap();
    let result: Result<KebabApp, _> = Clapfig::typed::<KebabApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .normalize_keys(true)
        .load();
    assert!(
        result.is_err(),
        "normalize_keys(true) + kebab-case schema must fail loudly"
    );
}

// -- Precedence and directional forms ---------------------------------------

/// Explicit renames (either attribute) exempt a field from the rule.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ExplicitWins {
    connect_timeout: u32,
    #[serde(rename = "legacy_name")]
    modern_name: u32,
}

#[test]
fn explicit_rename_overrides_the_rule() {
    let names: Vec<&str> = ExplicitWins::schema_static()
        .fields
        .iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["connectTimeout", "legacy_name"]);
}

#[test]
fn explicit_rename_overrides_the_rule_end_to_end() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("test.toml"),
        "connectTimeout = 7\nlegacy_name = 9\n",
    )
    .unwrap();
    let cfg: ExplicitWins = Clapfig::typed::<ExplicitWins>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.connect_timeout, 7);
    assert_eq!(cfg.modern_name, 9);
}

/// A serialize-only directional *field* rename is invisible to the
/// deserialize side, so the struct rule still applies to it — serde's
/// exact precedence.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
struct SerializeOnlyFieldRename {
    #[serde(rename(serialize = "WIRE_NAME"))]
    retry_count: u32,
}

#[test]
fn serialize_only_field_rename_still_gets_the_rule() {
    let s = SerializeOnlyFieldRename::schema_static();
    assert_eq!(s.fields[0].name, "retry-count");
}

/// Directional `rename_all(deserialize = ...)`: the schema follows the
/// deserialize rule; the serialize rule is irrelevant to config loading.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all(deserialize = "kebab-case", serialize = "SCREAMING_SNAKE_CASE"))]
struct DirectionalApp {
    read_timeout: u32,
}

#[test]
fn directional_rename_all_uses_deserialize_rule() {
    let s = DirectionalApp::schema_static();
    assert_eq!(s.fields[0].name, "read-timeout");
}

#[test]
fn directional_rename_all_loads_deserialize_spelling() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "read-timeout = 30\n").unwrap();
    let cfg: DirectionalApp = Clapfig::typed::<DirectionalApp>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.read_timeout, 30);
}

/// A serialize-only `rename_all(serialize = ...)` never touches the
/// deserialize side: accepted, and the schema keeps the Rust identifiers
/// (matching serde).
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all(serialize = "UPPERCASE"))]
struct SerializeOnlyRule {
    read_timeout: u32,
}

#[test]
fn serialize_only_rename_all_is_accepted_and_inert() {
    let s = SerializeOnlyRule::schema_static();
    assert_eq!(s.fields[0].name, "read_timeout");
}

/// The clapfig spelling works alone — the schema-only case, no serde
/// derive involved. This is the spelling's intended use: it converts the
/// *schema only* and cannot change a serde-generated `Deserialize`.
#[derive(Schema, Debug)]
#[clapfig(rename_all = "camelCase")]
struct ClapfigSpelling {
    #[allow(dead_code)]
    connect_timeout: u32,
}

#[test]
fn clapfig_rename_all_spelling_converts_field_names() {
    let s = ClapfigSpelling::schema_static();
    assert_eq!(s.fields[0].name, "connectTimeout");
}

/// The clapfig spelling on a struct that ALSO derives serde `Deserialize`
/// (without the matching serde attribute): the schema converts but serde
/// still expects the Rust identifiers, so a typed load of the converted
/// spelling fails — the documented reason typed structs must use
/// `#[serde(rename_all)]` (or a matching pair).
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[clapfig(rename_all = "camelCase")]
struct ClapfigOnlyTyped {
    connect_timeout: u32,
}

#[test]
fn clapfig_only_rule_on_typed_struct_converts_schema_but_not_deserialize() {
    // The schema side converts...
    let s = ClapfigOnlyTyped::schema_static();
    assert_eq!(s.fields[0].name, "connectTimeout");
    // ...but serde's generated Deserialize is untouched: validation
    // accepts the converted spelling, then typed deserialization looks
    // for `connect_timeout` and fails. Pinned so the docs' schema-only
    // caveat can't drift from reality.
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "connectTimeout = 7\n").unwrap();
    let result: Result<ClapfigOnlyTyped, _> = Clapfig::typed::<ClapfigOnlyTyped>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(
        result.is_err(),
        "clapfig-only rename_all must not make the typed path accept converted keys"
    );
}

/// Both spellings naming the same rule agree — no conflict, and the pair
/// is the documented way to keep the clapfig spelling on a typed struct.
#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
#[clapfig(rename_all = "camelCase")]
#[serde(rename_all = "camelCase")]
struct AgreeingPair {
    connect_timeout: u32,
}

#[test]
fn agreeing_clapfig_and_serde_rules_coexist() {
    let s = AgreeingPair::schema_static();
    assert_eq!(s.fields[0].name, "connectTimeout");
}

#[test]
fn agreeing_pair_loads_converted_spelling_end_to_end() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "connectTimeout = 7\n").unwrap();
    let cfg: AgreeingPair = Clapfig::typed::<AgreeingPair>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.connect_timeout, 7);
}
