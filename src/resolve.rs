//! Core resolution pipeline: merge all config layers and produce a typed config.
//!
//! Operates on pre-loaded data (`ResolveInput`) with no I/O, making the full
//! pipeline testable with synthetic inputs. Steps:
//!
//! 1. Validate each file (if strict mode)
//! 2. Parse and deep-merge config files (later overrides earlier)
//! 3. Deep-merge env vars on top
//! 4. Deep-merge CLI overrides on top (highest priority)
//! 5. Deserialize merged table into `C::Layer`
//! 6. Let confique fill defaults and validate required fields

use std::path::PathBuf;

use confique::Config;
use serde::Deserialize;
use toml::{Table, Value};

use crate::env;
use crate::error::ClapfigError;
use crate::merge::deep_merge;
use crate::overrides;
use crate::validate;

/// All pre-loaded data needed to resolve a config. No I/O happens here.
pub struct ResolveInput {
    /// File contents in precedence order: first = lowest priority, last = highest.
    pub files: Vec<(PathBuf, String)>,
    /// Raw environment variable pairs (pass `std::env::vars().collect()` or synthetic data).
    pub env_vars: Vec<(String, String)>,
    /// Env var prefix (e.g. `"MYAPP"`). `None` means env disabled.
    pub env_prefix: Option<String>,
    /// CLI overrides as `(dotted_key, value)` pairs.
    pub cli_overrides: Vec<(String, Value)>,
    /// Whether to reject unknown keys in config files.
    pub strict: bool,
}

/// Resolve configuration from pre-loaded inputs.
///
/// 1. Validate each file (if strict)
/// 2. Parse each file to `toml::Table`
/// 3. Deep-merge files (later overrides earlier)
/// 4. Deep-merge env table on top
/// 5. Deep-merge CLI overrides on top
/// 6. Deserialize merged table into `C::Layer`
/// 7. `C::builder().preloaded(layer).load()` — confique fills defaults and validates
pub fn resolve<C: Config>(input: ResolveInput) -> Result<C, ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
    // 1-3: Validate and merge file layers
    let mut merged = Table::new();
    for (path, content) in &input.files {
        if input.strict {
            validate::validate_unknown_keys::<C>(content, path)?;
        }
        let table: Table = toml::from_str(content).map_err(|e| ClapfigError::ParseError {
            path: path.clone(),
            source: e,
        })?;
        merged = deep_merge(merged, table);
    }

    // 4: Env vars on top
    if let Some(prefix) = &input.env_prefix {
        let env_table = env::env_to_table(prefix, input.env_vars);
        merged = deep_merge(merged, env_table);
    }

    // 5: CLI overrides on top (highest priority)
    if !input.cli_overrides.is_empty() {
        let cli_table = overrides::overrides_to_table(&input.cli_overrides);
        merged = deep_merge(merged, cli_table);
    }

    // 6: Deserialize merged table directly into C::Layer
    let layer: C::Layer = Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| ClapfigError::InvalidValue {
            key: "<merged>".into(),
            reason: e.to_string(),
        })?;

    // 7: confique fills defaults and validates required fields
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
            cli_overrides: vec![],
            strict: true,
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
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            strict: true,
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
            cli_overrides: vec![("debug".into(), Value::Boolean(true))],
            strict: true,
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
}
