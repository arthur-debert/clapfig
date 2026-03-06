//! Core resolution pipeline: merge all config layers and produce a typed config.
//!
//! Operates on pre-loaded data (`ResolveInput`) with no I/O, making the full
//! pipeline testable with synthetic inputs. Steps:
//!
//! 1. Build each layer independently (files, env, URL, CLI)
//! 2. Merge layers in the configured order (default: files < env < URL < CLI)
//! 3. Deserialize merged table into `C::Layer`
//! 4. Let confique fill defaults and validate required fields
//!
//! The layer order is configurable via [`ResolveInput::layer_order`].

use std::path::PathBuf;

use confique::Config;
use serde::Deserialize;
use toml::{Table, Value};

use crate::env;
use crate::error::ClapfigError;
use crate::merge::deep_merge;
use crate::overrides;
use crate::types::Layer;
use crate::validate;

/// All pre-loaded data needed to resolve a config. No I/O happens here.
pub struct ResolveInput {
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
    /// Layer merge order, from lowest to highest priority.
    /// `None` uses the default: `[Files, Env, Url, Cli]`.
    pub layer_order: Option<Vec<Layer>>,
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
/// (default: files < env < URL < CLI), then deserializes into `C`.
pub fn resolve<C: Config>(input: ResolveInput) -> Result<C, ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
    // Build each layer independently, then merge in the configured order.

    // Files layer: validate + parse + merge all file contents
    let files_table = {
        let mut t = Table::new();
        for (path, content) in &input.files {
            if input.strict {
                validate::validate_unknown_keys::<C>(content, path)?;
            }
            let table: Table = toml::from_str(content).map_err(|e| ClapfigError::ParseError {
                path: path.clone(),
                source: e,
            })?;
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
        Some(overrides::overrides_to_table(&input.url_overrides))
    };

    // CLI layer
    let cli_table = if input.cli_overrides.is_empty() {
        None
    } else {
        Some(overrides::overrides_to_table(&input.cli_overrides))
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

    // Deserialize merged table directly into C::Layer
    let layer: C::Layer = Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| ClapfigError::InvalidValue {
            key: "<merged>".into(),
            reason: e.to_string(),
        })?;

    // confique fills defaults and validates required fields
    C::builder()
        .preloaded(layer)
        .load()
        .map_err(ClapfigError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;

    fn empty_input() -> ResolveInput {
        ResolveInput {
            files: vec![],
            env_vars: vec![],
            env_prefix: None,
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![],
            strict: true,
            layer_order: None,
        }
    }

    #[test]
    fn defaults_only() {
        let config: TestConfig = resolve(empty_input()).unwrap();
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 5000);
    }

    #[test]
    fn cli_overrides_all() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            strict: true,
            layer_order: None,
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 9999);
    }

    #[test]
    fn sparse_merge_across_layers() {
        let input = ResolveInput {
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 3000);
    }

    // -- deserialize_with normalization tests ----------------------------------

    use crate::fixtures::test::NormalizedConfig;

    #[test]
    fn deserialize_with_normalizes_from_file() {
        let input = ResolveInput {
            files: vec![("test.toml".into(), "color = \"BLUE\"\n".into())],
            ..empty_input()
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "blue");
    }

    #[test]
    fn deserialize_with_normalizes_from_env() {
        let input = ResolveInput {
            env_vars: vec![("APP__COLOR".into(), "GREEN".into())],
            env_prefix: Some("APP".into()),
            ..empty_input()
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "green");
    }

    #[test]
    fn deserialize_with_normalizes_from_cli_override() {
        let input = ResolveInput {
            cli_overrides: vec![("color".into(), Value::String("MAGENTA".into()))],
            ..empty_input()
        };
        let config: NormalizedConfig = resolve(input).unwrap();
        assert_eq!(config.color, "magenta");
    }

    #[test]
    fn deserialize_with_default_is_not_normalized() {
        // confique defaults bypass deserialize_with — they are injected directly.
        // This is confique's documented behavior: the default string is used as-is.
        let config: NormalizedConfig = resolve(empty_input()).unwrap();
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
            ..empty_input()
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
            ..empty_input()
        };
        let config: TestConfig = resolve(input).unwrap();
        assert_eq!(config.port, 9999);
    }

    #[cfg(feature = "url")]
    #[test]
    fn url_nested_key() {
        let input = ResolveInput {
            url_overrides: vec![("database.pool_size".into(), Value::Integer(42))],
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
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
            ..empty_input()
        };
        let config: TestConfig = resolve(input).unwrap();
        // Url is last → highest priority
        assert_eq!(config.port, 7777);
    }
}
