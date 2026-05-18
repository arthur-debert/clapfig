//! Core resolution pipeline: merge all config layers and produce a typed config.
//!
//! Operates on pre-loaded data (`ResolveInput`) with no I/O, making the full
//! pipeline testable with synthetic inputs. Steps:
//!
//! 1. Build each layer independently (files, env, URL, CLI)
//! 2. Merge layers in the configured order (default: files < env < URL < CLI)
//! 3. Hand the merged table to a [`ConfigSpec`] for finalization (defaults +
//!    required-field check + typed output)
//!
//! The layer order is configurable via [`ResolveInput::layer_order`]. The
//! spec parameter decouples this pipeline from the compile-time `Config`
//! derive: the static path supplies `StaticSpec<C>`, and Phase 2 will add a
//! runtime path that supplies a `DynamicSpec`.

use std::path::PathBuf;
use std::sync::Arc;

use toml::{Table, Value};

use crate::env;
use crate::error::ClapfigError;
use crate::merge::deep_merge;
use crate::normalize::{normalize_key, normalize_table};
use crate::overrides;
use crate::spec::ConfigSpec;
use crate::types::Layer;

/// All pre-loaded data needed to resolve a config. No I/O happens here.
///
/// Generic over [`ConfigSpec`]: the static path threads in `StaticSpec<C>`;
/// the planned runtime path (issue #36) will thread in a `DynamicSpec`.
pub struct ResolveInput<'a, S: ConfigSpec> {
    /// Schema-walking strategy: validate unknown keys, finalize the merged
    /// table into the spec's `Output`.
    pub spec: &'a S,
    /// File contents in precedence order: first = lowest priority, last = highest.
    pub files: Vec<(PathBuf, String)>,
    /// Raw environment variable pairs (pass `std::env::vars().collect()` or synthetic data).
    pub env_vars: Vec<(String, String)>,
    /// Env var prefix (e.g. `"MYAPP"`). `None` means env disabled.
    pub env_prefix: Option<String>,
    /// URL query parameter overrides as `(dotted_key, value)` pairs.
    #[cfg(feature = "url")]
    pub url_overrides: Vec<(String, Value)>,
    /// CLI overrides as `(dotted_key, value)` pairs.
    pub cli_overrides: Vec<(String, Value)>,
    /// Whether to reject unknown keys in config files.
    pub strict: bool,
    /// Whether to rewrite `-` to `_` in every key supplied by the user
    /// (config files, CLI overrides, URL overrides) before validation and
    /// merging — letting kebab-case keys map to snake_case Rust fields.
    pub normalize_keys: bool,
    /// Layer merge order, from lowest to highest priority.
    /// `None` uses the default: `[Files, Env, Url, Cli]`.
    pub layer_order: Option<Vec<Layer>>,
}

/// Rewrite the dotted-key half of each override pair, applying the same
/// `-` → `_` rule as [`normalize_table`]. Used so CLI/URL-supplied keys land
/// in the same shape as keys coming from normalized config files.
fn normalize_override_keys(
    entries: &[(String, toml::Value)],
    normalize_keys: bool,
) -> Vec<(String, toml::Value)> {
    if !normalize_keys {
        return entries.to_vec();
    }
    entries
        .iter()
        .map(|(k, v)| (normalize_key(k), v.clone()))
        .collect()
}

/// Returns the default layer order: `[Files, Env, Url, Cli]`.
pub fn default_layer_order() -> Vec<Layer> {
    vec![
        Layer::Files,
        Layer::Env,
        #[cfg(feature = "url")]
        Layer::Url,
        Layer::Cli,
    ]
}

/// Resolve configuration from pre-loaded inputs.
///
/// Builds each layer independently, merges them in the configured order
/// (default: files < env < URL < CLI), then hands the merged table to the
/// spec for finalization.
pub fn resolve<S: ConfigSpec>(input: ResolveInput<'_, S>) -> Result<S::Output, ClapfigError> {
    // Build each layer independently, then merge in the configured order.

    // Files layer: parse → (optionally) normalize → validate → merge.
    // Validation runs against the parsed Table — never the raw text — so
    // normalized keys are checked in the same form they will reach the merge.
    let files_table = {
        let mut t = Table::new();
        for (path, content) in &input.files {
            let mut table: Table =
                toml::from_str(content).map_err(|e| ClapfigError::ParseError {
                    path: path.clone(),
                    source: Box::new(e),
                    source_text: Some(Arc::from(content.as_str())),
                })?;
            if input.normalize_keys {
                normalize_table(&mut table).map_err(|c| ClapfigError::NormalizedKeyCollision {
                    path: path.clone(),
                    section: c.section,
                    normalized_key: c.normalized_key,
                    originals: c.originals,
                })?;
            }
            if input.strict {
                input
                    .spec
                    .validate_unknown(&table, content, path, input.normalize_keys)?;
            }
            t = deep_merge(t, table);
        }
        t
    };

    // Env layer
    let env_table = input
        .env_prefix
        .as_ref()
        .map(|prefix| env::env_to_table(prefix, input.env_vars));

    // URL layer
    #[cfg(feature = "url")]
    let url_table = if input.url_overrides.is_empty() {
        None
    } else {
        Some(overrides::overrides_to_table(&normalize_override_keys(
            &input.url_overrides,
            input.normalize_keys,
        )))
    };

    // CLI layer
    let cli_table = if input.cli_overrides.is_empty() {
        None
    } else {
        Some(overrides::overrides_to_table(&normalize_override_keys(
            &input.cli_overrides,
            input.normalize_keys,
        )))
    };

    // Default order: Files < Env < Url < Cli
    let default_order = default_layer_order();
    let order = input.layer_order.as_deref().unwrap_or(&default_order);

    // Merge layers in the specified order (first = lowest priority)
    let mut merged = Table::new();
    for layer in order {
        let table = match layer {
            Layer::Files => Some(files_table.clone()),
            Layer::Env => env_table.clone(),
            #[cfg(feature = "url")]
            Layer::Url => url_table.clone(),
            Layer::Cli => cli_table.clone(),
        };
        if let Some(t) = table {
            merged = deep_merge(merged, t);
        }
    }

    // Spec-driven default injection. No-op for the static path (confique
    // injects defaults inside `finalize`); the runtime path populates the
    // table here so `finalize` only has to check required fields.
    input.spec.fill_defaults(&mut merged)?;

    input.spec.finalize(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::{NormalizedConfig, TestConfig};
    use crate::spec::StaticSpec;

    // `StaticSpec` is ZST and `new()` is const, so a module-level constant
    // lets every test reference `&TEST_SPEC` / `&NORM_SPEC` without spelling
    // out a per-test `let spec = ...` line.
    const TEST_SPEC: StaticSpec<TestConfig> = StaticSpec::new();
    const NORM_SPEC: StaticSpec<NormalizedConfig> = StaticSpec::new();

    fn empty_input<'a, S: ConfigSpec>(spec: &'a S) -> ResolveInput<'a, S> {
        ResolveInput {
            spec,
            files: vec![],
            env_vars: vec![],
            env_prefix: None,
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![],
            strict: true,
            normalize_keys: false,
            layer_order: None,
        }
    }

    #[test]
    fn defaults_only() {
        let config: TestConfig = resolve(empty_input(&TEST_SPEC)).unwrap();
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 8080);
        assert!(!config.debug);
        assert_eq!(config.database.pool_size, 5);
        assert_eq!(config.database.url, None);
    }

    #[test]
    fn file_overrides_default() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 3000);
        assert_eq!(config.host, "localhost"); // default preserved
    }

    #[test]
    fn later_file_overrides_earlier() {
        let input = ResolveInput {
            files: vec![
                ("first.toml".into(), "port = 1000\n".into()),
                ("second.toml".into(), "port = 2000\n".into()),
            ],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 2000);
    }

    #[test]
    fn env_overrides_file() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 5000);
    }

    #[test]
    fn cli_overrides_all() {
        let input = ResolveInput {
            spec: &TEST_SPEC,
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            strict: true,
            normalize_keys: false,
            layer_order: None,
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 9999);
    }

    #[test]
    fn sparse_merge_across_layers() {
        let input = ResolveInput {
            spec: &TEST_SPEC,
            files: vec![(
                "test.toml".into(),
                "host = \"filehost\"\n[database]\npool_size = 20\n".into(),
            )],
            env_vars: vec![("APP__PORT".into(), "4000".into())],
            env_prefix: Some("APP".into()),
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![("debug".into(), Value::Boolean(true))],
            strict: true,
            normalize_keys: false,
            layer_order: None,
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.host, "filehost"); // from file
        assert_eq!(config.port, 4000); // from env
        assert!(config.debug); // from cli
        assert_eq!(config.database.pool_size, 20); // from file
    }

    #[test]
    fn nested_file_merge() {
        let input = ResolveInput {
            files: vec![
                (
                    "base.toml".into(),
                    "[database]\nurl = \"pg://base\"\npool_size = 5\n".into(),
                ),
                ("local.toml".into(), "[database]\npool_size = 50\n".into()),
            ],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.url.as_deref(), Some("pg://base")); // from base
        assert_eq!(config.database.pool_size, 50); // overridden by local
    }

    #[test]
    fn strict_rejects_unknown_key() {
        let input = ResolveInput {
            files: vec![("bad.toml".into(), "typo = 1\n".into())],
            strict: true,
            ..empty_input(&TEST_SPEC)
        };
        let result: Result<TestConfig, _> = resolve(input);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("typo") || msg.contains("Unknown"));
    }

    #[test]
    fn lenient_allows_unknown_key() {
        let input = ResolveInput {
            files: vec![("ok.toml".into(), "typo = 1\nport = 3000\n".into())],
            strict: false,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 3000);
    }

    // -- deserialize_with normalization tests ----------------------------------

    #[test]
    fn deserialize_with_normalizes_from_file() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "color = \"BLUE\"\n".into())],
            ..empty_input(&NORM_SPEC)
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "blue");
    }

    #[test]
    fn deserialize_with_normalizes_from_env() {
        let input = ResolveInput {
            env_vars: vec![("APP__COLOR".into(), "GREEN".into())],
            env_prefix: Some("APP".into()),
            ..empty_input(&NORM_SPEC)
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "green");
    }

    #[test]
    fn deserialize_with_normalizes_from_cli_override() {
        let input = ResolveInput {
            cli_overrides: vec![("color".into(), Value::String("MAGENTA".into()))],
            ..empty_input(&NORM_SPEC)
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "magenta");
    }

    #[test]
    fn deserialize_with_default_is_not_normalized() {
        // confique defaults bypass deserialize_with — they are injected directly.
        // This is confique's documented behavior: the default string is used as-is.
        let config: NormalizedConfig = resolve(empty_input(&NORM_SPEC)).unwrap();
        assert_eq!(config.color, "red");
    }

    // -- URL layer precedence tests -------------------------------------------

    #[cfg(feature = "url")]
    #[test]
    fn url_overrides_env() {
        let input = ResolveInput {
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 7777);
    }

    #[cfg(feature = "url")]
    #[test]
    fn cli_overrides_url() {
        let input = ResolveInput {
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 9999);
    }

    #[cfg(feature = "url")]
    #[test]
    fn url_nested_key() {
        let input = ResolveInput {
            url_overrides: vec![("database.pool_size".into(), Value::Integer(42))],
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.pool_size, 42);
    }

    // -- Custom layer order tests ---------------------------------------------

    #[test]
    fn custom_order_env_overrides_cli() {
        // Reverse the usual CLI > Env precedence
        let input = ResolveInput {
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![Layer::Cli, Layer::Env]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // Env comes after Cli in the order, so Env wins
        assert_eq!(config.port, 5000);
    }

    #[test]
    fn custom_order_files_override_env() {
        // Make files win over env
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            layer_order: Some(vec![Layer::Env, Layer::Files]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // Files come after Env, so Files win
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn custom_order_omitted_layer_excluded() {
        // Omit Env layer entirely — env vars should have no effect
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            layer_order: Some(vec![Layer::Files, Layer::Cli]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // Env is not in layer_order, so the file value stands
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn custom_order_cli_only() {
        // Only CLI layer
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(7777))],
            layer_order: Some(vec![Layer::Cli]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 7777);
    }

    #[test]
    fn custom_order_empty_uses_only_defaults() {
        // Empty layer order — no layers merged, only confique defaults
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // No layers applied, so confique default (8080) stands
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn default_order_preserved_when_none() {
        // layer_order: None should behave exactly like the old hardcoded order
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: None,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 9999); // CLI wins
    }

    #[test]
    fn custom_order_all_three_sources_reordered() {
        // Order: Cli < Files < Env (env has highest priority)
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "host = \"filehost\"\nport = 3000\n".into(),
            )],
            env_vars: vec![("APP__PORT".into(), "5000".into())],
            env_prefix: Some("APP".into()),
            cli_overrides: vec![
                ("port".into(), Value::Integer(9999)),
                ("debug".into(), Value::Boolean(true)),
            ],
            layer_order: Some(vec![Layer::Cli, Layer::Files, Layer::Env]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // Env is last → highest priority for port
        assert_eq!(config.port, 5000);
        // Files overrides Cli for host (file has it, cli doesn't set host)
        assert_eq!(config.host, "filehost");
        // debug only set in Cli (lowest here), but no other layer overrides it
        assert!(config.debug);
    }

    #[cfg(feature = "url")]
    #[test]
    fn custom_order_url_highest_priority() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![Layer::Files, Layer::Env, Layer::Cli, Layer::Url]),
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        // Url is last → highest priority
        assert_eq!(config.port, 7777);
    }

    // -- normalize_keys tests -------------------------------------------------

    #[test]
    fn normalize_off_kebab_file_key_rejected_by_strict() {
        // Baseline: without normalization, a kebab key in a config file is a
        // strict-mode violation. Locks the opt-in behavior.
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-size = 25\n".into())],
            ..empty_input(&TEST_SPEC)
        };
        let result: Result<TestConfig, _> = resolve(input);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_on_kebab_file_key_accepted() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-size = 25\n".into())],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.pool_size, 25);
    }

    #[test]
    fn normalize_on_snake_file_key_still_works() {
        // Backwards-compatible: snake-cased keys keep working when
        // normalization is on.
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool_size = 30\n".into())],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.pool_size, 30);
    }

    #[test]
    fn normalize_on_kebab_cli_override_accepted() {
        let input = ResolveInput {
            cli_overrides: vec![("database.pool-size".into(), Value::Integer(77))],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.pool_size, 77);
    }

    #[cfg(feature = "url")]
    #[test]
    fn normalize_on_kebab_url_override_accepted() {
        let input = ResolveInput {
            url_overrides: vec![("database.pool-size".into(), Value::Integer(88))],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.database.pool_size, 88);
    }

    #[test]
    fn normalize_on_kebab_typo_still_strict_errors() {
        // Normalization isn't a free pass — a kebab-cased *typo* still gets
        // flagged because the snake form is also unknown.
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-zize = 25\n".into())],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let result: Result<TestConfig, _> = resolve(input);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        // Reported in normalized form.
        assert_eq!(keys[0].key, "database.pool_zize");
    }

    #[test]
    fn normalize_on_collision_in_file_errors() {
        // Two distinct keys in the same table that normalize to the same
        // name must surface as an explicit error, not silently drop one.
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "[database]\npool-size = 5\npool_size = 10\n".into(),
            )],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let result: Result<TestConfig, _> = resolve(input);
        match result {
            Err(ClapfigError::NormalizedKeyCollision {
                normalized_key,
                section,
                originals,
                ..
            }) => {
                assert_eq!(normalized_key, "pool_size");
                assert_eq!(section, "database");
                assert_eq!(originals, vec!["pool-size", "pool_size"]);
            }
            other => panic!("expected NormalizedKeyCollision, got {other:?}"),
        }
    }

    #[test]
    fn normalize_on_mixed_kebab_and_snake_in_same_file() {
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "host = \"x\"\n[database]\npool-size = 10\nurl = \"pg://y\"\n".into(),
            )],
            normalize_keys: true,
            ..empty_input(&TEST_SPEC)
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.host, "x");
        assert_eq!(config.database.pool_size, 10);
        assert_eq!(config.database.url.as_deref(), Some("pg://y"));
    }
}
