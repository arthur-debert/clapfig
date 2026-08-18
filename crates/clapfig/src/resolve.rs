//! Core resolution pipeline: merge all config layers and produce a merged
//! config table.
//!
//! Operates on pre-loaded data (`ResolveInput`) with no I/O, making the full
//! pipeline testable with synthetic inputs. Steps:
//!
//! 1. Build each layer independently (files, env, URL, CLI)
//! 2. Merge layers in the configured order (default: files < env < URL < CLI)
//! 3. Hand the merged table to a [`ConfigSpec`] for finalization (defaults +
//!    required-field and type checks)
//!
//! The layer order is configurable via [`ResolveInput::layer_order`]. The
//! spec parameter decouples this pipeline from the schema source: both
//! `Clapfig::runtime(schema)` and the derive-driven
//! `Clapfig::schema_builder::<C>()` thread in a `DynamicSpec`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::env;
use crate::error::ClapfigError;
use crate::format::{self, FormatRegistry};
use crate::merge::deep_merge;
use crate::normalize::{normalize_key, normalize_table};
use crate::overrides;
use crate::spec::ConfigSpec;
use crate::strict::{CollectedUnknown, StrictnessOverrides, UnknownKeyHook};
use crate::types::Layer;
use crate::validate::ValidateContext;
use crate::value::{Map, Value};

/// All pre-loaded data needed to resolve a config. No I/O happens here.
///
/// Generic over [`ConfigSpec`], keeping the pipeline decoupled from how the
/// schema was produced (runtime builder or derive macro).
pub(crate) struct ResolveInput<'a, S: ConfigSpec> {
    /// Schema-walking strategy: validate unknown keys, finalize the merged
    /// table into the spec's `Output`.
    pub spec: &'a S,
    /// Enabled format adapters — the routing seam every file parse goes
    /// through. Per-file adapter selection is by extension; extensionless
    /// files (rc-style names) fall back to the preferred
    /// (first-registered) adapter, and an extension no enabled adapter
    /// claims is a hard [`ClapfigError::UnknownFormat`] — the same
    /// explicit-path rule as persist targets and `gen --output`, never a
    /// silent parse under another format.
    pub registry: &'a FormatRegistry,
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
    /// Builder-level default strictness — applies to any unknown key with
    /// no explicit override on an ancestor in `strict_overrides`. Existing
    /// callers' `strict(bool)` setting flows through here unchanged.
    pub strict_default: bool,
    /// Cascading strictness overrides — Phase 3 (#37). Empty when no
    /// runtime `Schema::strict` or builder `strict_at` has been registered,
    /// at which point every unknown key is decided by `strict_default`.
    pub strict_overrides: StrictnessOverrides,
    /// Optional per-key callback registered by `on_unknown_key`. Runs only
    /// on keys the cascade flags strict; `Accept` drops them silently,
    /// `Reject` produces a `ClapfigError::UnknownKeys` entry.
    pub unknown_key_hook: Option<UnknownKeyHook>,
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
    entries: &[(String, Value)],
    normalize_keys: bool,
) -> Vec<(String, Value)> {
    if !normalize_keys {
        return entries.to_vec();
    }
    entries
        .iter()
        .map(|(k, v)| (normalize_key(k), v.clone()))
        .collect()
}

/// Returns the default layer order: `[Files, Env, Url, Cli]`.
pub(crate) fn default_layer_order() -> Vec<Layer> {
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
/// spec for finalization. Returns the typed output plus any keys the
/// `on_unknown_key` callback elected to
/// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect);
/// callers that don't need the collected list (the plain `load()`
/// surface) simply discard it via `let (out, _) = resolve(...)?;`.
pub(crate) fn resolve<S: ConfigSpec>(
    input: ResolveInput<'_, S>,
) -> Result<(S::Output, Vec<CollectedUnknown>), ClapfigError> {
    // Build each layer independently, then merge in the configured order.

    let validate_ctx = ValidateContext {
        overrides: &input.strict_overrides,
        default_strict: input.strict_default,
        callback: input.unknown_key_hook.as_ref(),
        normalize_keys: input.normalize_keys,
    };

    // Skip validation entirely when no path through the cascade can ever
    // resolve to strict: builder default `strict(false)` AND no override
    // sets `strict = true`. Lenient subtrees alone (e.g.
    // `strict(false).strict_at("section", false)`) still trigger no
    // strict outcome anywhere, so the per-file walk would only ever
    // drop keys silently — pure wasted work.
    //
    // The `on_unknown_key` callback is irrelevant to this check: it
    // only fires on cascade-strict keys, so a cascade that produces no
    // strict outcomes never calls it.
    let cascade_active = input.strict_default || input.strict_overrides.has_any_strict();

    // Files layer: parse → (optionally) normalize → validate → merge.
    // Validation runs against the parsed Table — never the raw text — so
    // normalized keys are checked in the same form they will reach the merge.
    let mut collected_unknowns: Vec<CollectedUnknown> = Vec::new();
    let files_table = {
        let mut t = Map::new();
        for (path, content) in &input.files {
            // Extensionless (rc-style) names fall back to the preferred
            // adapter; an extension no enabled adapter claims is a hard
            // UnknownFormat — the documented explicit-path rule, never a
            // silent parse under another format.
            let adapter = match path.extension() {
                None => input
                    .registry
                    .preferred()
                    .ok_or_else(|| ClapfigError::UnknownFormat {
                        name: path.display().to_string(),
                        available: format::builtin_names(),
                    })?,
                Some(ext) => {
                    let ext = ext.to_string_lossy();
                    input.registry.by_extension(&ext).ok_or_else(|| {
                        ClapfigError::UnknownFormat {
                            name: ext.into_owned(),
                            available: format::builtin_names(),
                        }
                    })?
                }
            };
            let parsed = adapter
                .parse(content)
                .map_err(|e| ClapfigError::ParseError {
                    path: path.clone(),
                    source: Box::new(e),
                    source_text: Some(Arc::from(content.as_str())),
                })?;
            let mut table = match parsed {
                Value::Map(map) => map,
                other => {
                    return Err(ClapfigError::InvalidValue {
                        key: path.display().to_string(),
                        reason: format!(
                            "config documents must be maps at the root, got {}",
                            other.type_str()
                        ),
                    });
                }
            };
            if input.normalize_keys {
                normalize_table(&mut table).map_err(|c| ClapfigError::NormalizedKeyCollision {
                    path: path.clone(),
                    section: c.section,
                    normalized_key: c.normalized_key,
                    originals: c.originals,
                })?;
            }
            if cascade_active {
                let mut per_file =
                    input
                        .spec
                        .validate_unknown(&table, content, path, &validate_ctx)?;
                collected_unknowns.append(&mut per_file);
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

    // Validate the env-derived table against the schema. Before this pass
    // env-unknown keys merged in unnoticed: `validate_unknown` only ran
    // per-file, the env layer landed afterwards, and `finalize` doesn't
    // re-check unknown keys. Now an env var like `MYAPP__ROGUE` flows
    // through the same cascade (and `on_unknown_key` callback) as a
    // file-supplied `rogue = 1` key, with the source path rendered as
    // `<env>` and no line number.
    //
    // The walker is schema-driven (no typed deserialize) so type
    // mismatches between env's heuristic value parsing (e.g. string
    // "1.5" for an integer field) don't fail validation — that's
    // still the job of the final-merge type check inside `finalize`.
    if cascade_active && let Some(env_table_ref) = env_table.as_ref() {
        let env_unknowns =
            crate::validate::collect_unknown_paths_ref(env_table_ref, input.spec.schema(), "");
        let env_path = std::path::PathBuf::from("<env>");
        let mut env_filtered = crate::validate::filter_through_cascade(
            env_table_ref,
            "",
            &env_path,
            env_unknowns,
            &validate_ctx,
        )?;
        collected_unknowns.append(&mut env_filtered);
    }

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
    let mut merged = Map::new();
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

    // Spec-driven default injection: populate the table from the schema's
    // declared defaults so `finalize` only has to check required fields.
    input.spec.fill_defaults(&mut merged)?;

    let output = input.spec.finalize(merged)?;
    Ok((output, collected_unknowns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;
    use crate::runtime_spec::DynamicSpec;

    fn test_spec() -> DynamicSpec {
        DynamicSpec::new(test_schema())
    }

    fn toml_only_registry() -> &'static FormatRegistry {
        use std::sync::OnceLock;
        static REGISTRY: OnceLock<FormatRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let mut r = FormatRegistry::new();
            r.register(Box::new(crate::format::TomlAdapter));
            r
        })
    }

    fn empty_input<S: ConfigSpec>(spec: &S) -> ResolveInput<'_, S> {
        ResolveInput {
            spec,
            registry: toml_only_registry(),
            files: vec![],
            env_vars: vec![],
            env_prefix: None,
            #[cfg(feature = "url")]
            url_overrides: vec![],
            cli_overrides: vec![],
            strict_default: true,
            strict_overrides: StrictnessOverrides::new(),
            unknown_key_hook: None,
            normalize_keys: false,
            layer_order: None,
        }
    }

    fn get<'a>(table: &'a Map, dotted: &str) -> Option<&'a Value> {
        crate::ops::table_get(table, dotted)
    }

    #[test]
    fn defaults_only() {
        let spec = test_spec();
        let (table, _) = resolve(empty_input(&spec)).unwrap();
        assert_eq!(get(&table, "host").unwrap().as_str(), Some("localhost"));
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(8080));
        assert_eq!(get(&table, "debug").unwrap().as_bool(), Some(false));
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(5)
        );
        assert!(get(&table, "database.url").is_none());
    }

    #[test]
    fn file_overrides_default() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
        // default preserved
        assert_eq!(get(&table, "host").unwrap().as_str(), Some("localhost"));
    }

    #[test]
    fn unclaimed_extension_is_unknown_format() {
        // The explicit-path rule holds at the pipeline seam too: a file
        // whose extension no enabled adapter claims is a hard
        // UnknownFormat, never a silent parse under the preferred format.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("config.ini".into(), "port = 3000\n".into())],
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        match err {
            ClapfigError::UnknownFormat { name, .. } => assert_eq!(name, "ini"),
            other => panic!("expected UnknownFormat, got {other:?}"),
        }
    }

    #[test]
    fn extensionless_file_falls_back_to_preferred() {
        // Rc-style extensionless names are the documented UnknownFormat
        // carve-out: they parse under the preferred (first-registered)
        // adapter.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![(".myapprc".into(), "port = 3000\n".into())],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
    }

    #[test]
    fn later_file_overrides_earlier() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![
                ("first.toml".into(), "port = 1000\n".into()),
                ("second.toml".into(), "port = 2000\n".into()),
            ],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(2000));
    }

    #[test]
    fn env_overrides_file() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(5000));
    }

    #[test]
    fn cli_overrides_all() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(9999));
    }

    #[test]
    fn sparse_merge_across_layers() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "host = \"filehost\"\n[database]\npool_size = 20\n".into(),
            )],
            env_vars: vec![("APP__PORT".into(), "4000".into())],
            env_prefix: Some("APP".into()),
            cli_overrides: vec![("debug".into(), Value::Boolean(true))],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "host").unwrap().as_str(), Some("filehost")); // from file
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(4000)); // from env
        assert_eq!(get(&table, "debug").unwrap().as_bool(), Some(true)); // from cli
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(20)
        ); // from file
    }

    #[test]
    fn nested_file_merge() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![
                (
                    "base.toml".into(),
                    "[database]\nurl = \"pg://base\"\npool_size = 5\n".into(),
                ),
                ("local.toml".into(), "[database]\npool_size = 50\n".into()),
            ],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.url").unwrap().as_str(),
            Some("pg://base")
        ); // from base
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(50)
        ); // overridden by local
    }

    #[test]
    fn merged_type_error_reports_offending_key() {
        // Post-merge type checking names the exact key, not an opaque
        // "<merged>" placeholder.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = \"not-a-number\"\n".into())],
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, .. } => assert_eq!(key, "port"),
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn strict_rejects_unknown_key() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("bad.toml".into(), "typo = 1\n".into())],
            ..empty_input(&spec)
        };
        let result = resolve(input);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("typo") || msg.contains("Unknown"));
    }

    #[test]
    fn env_unknown_key_rejected_when_strict() {
        // Issue #54 item 3: env-derived unknown keys used to merge in
        // unnoticed. They now flow through the same cascade as file
        // keys — `MYAPP__ROGUE=1` errors the same way `rogue = 1` in a
        // file would.
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__ROGUE_KEY".into(), "1".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "rogue_key");
    }

    #[test]
    fn env_unknown_key_invokes_on_unknown_key_callback() {
        // The callback fires on env-derived unknowns the same as file
        // unknowns — closing the gap where dotted-extension keys from
        // the env layer reached the merge without ever being seen by
        // user code.
        let saw_env_unknown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw = std::sync::Arc::clone(&saw_env_unknown);
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__ROGUE_KEY".into(), "1".into())],
            env_prefix: Some("MYAPP".into()),
            unknown_key_hook: Some(std::sync::Arc::new(move |ctx| {
                if ctx.path == "rogue_key" {
                    saw.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                crate::strict::UnknownKeyDecision::Accept
            })),
            ..empty_input(&spec)
        };
        let result = resolve(input);
        assert!(result.is_ok());
        assert!(
            saw_env_unknown.load(std::sync::atomic::Ordering::SeqCst),
            "on_unknown_key must run for env-derived unknown keys"
        );
    }

    #[test]
    fn env_unknown_key_dropped_under_lenient_subtree() {
        // Cascade still applies: `strict_at("section", false)` makes that
        // subtree lenient for env-derived keys too.
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("database", false);
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("APP__DATABASE__ROGUE".into(), "x".into())],
            env_prefix: Some("APP".into()),
            strict_overrides: overrides,
            ..empty_input(&spec)
        };
        // Should load cleanly — the cascade marks `database.*` lenient.
        let result = resolve(input);
        assert!(result.is_ok(), "lenient subtree applies to env: {result:?}");
    }

    #[test]
    fn env_layer_does_not_re_validate_when_cascade_inactive() {
        // No strict default, no strict overrides → validation is skipped
        // entirely, both for files AND for the env layer. Unknown env vars
        // pass through silently.
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("APP__ROGUE_KEY".into(), "1".into())],
            env_prefix: Some("APP".into()),
            strict_default: false,
            ..empty_input(&spec)
        };
        let result = resolve(input);
        assert!(result.is_ok());
    }

    #[test]
    fn lenient_allows_unknown_key() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("ok.toml".into(), "typo = 1\nport = 3000\n".into())],
            strict_default: false,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
    }

    // -- URL layer precedence tests -------------------------------------------

    #[cfg(feature = "url")]
    #[test]
    fn url_overrides_env() {
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(7777));
    }

    #[cfg(feature = "url")]
    #[test]
    fn cli_overrides_url() {
        let spec = test_spec();
        let input = ResolveInput {
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(9999));
    }

    #[cfg(feature = "url")]
    #[test]
    fn url_nested_key() {
        let spec = test_spec();
        let input = ResolveInput {
            url_overrides: vec![("database.pool_size".into(), Value::Integer(42))],
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(42)
        );
    }

    // -- Custom layer order tests ---------------------------------------------

    #[test]
    fn custom_order_env_overrides_cli() {
        // Reverse the usual CLI > Env precedence
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![Layer::Cli, Layer::Env]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // Env comes after Cli in the order, so Env wins
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(5000));
    }

    #[test]
    fn custom_order_files_override_env() {
        // Make files win over env
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            layer_order: Some(vec![Layer::Env, Layer::Files]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // Files come after Env, so Files win
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
    }

    #[test]
    fn custom_order_omitted_layer_excluded() {
        // Omit Env layer entirely — env vars should have no effect
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            layer_order: Some(vec![Layer::Files, Layer::Cli]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // Env is not in layer_order, so the file value stands
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
    }

    #[test]
    fn custom_order_cli_only() {
        // Only CLI layer
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(7777))],
            layer_order: Some(vec![Layer::Cli]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(7777));
    }

    #[test]
    fn custom_order_empty_uses_only_defaults() {
        // Empty layer order — no layers merged, only schema defaults
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // No layers applied, so the schema default (8080) stands
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(8080));
    }

    #[test]
    fn default_order_preserved_when_none() {
        // layer_order: None should behave exactly like the hardcoded order
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: None,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(9999)); // CLI wins
    }

    #[test]
    fn custom_order_all_three_sources_reordered() {
        // Order: Cli < Files < Env (env has highest priority)
        let spec = test_spec();
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
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // Env is last → highest priority for port
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(5000));
        // Files overrides Cli for host (file has it, cli doesn't set host)
        assert_eq!(get(&table, "host").unwrap().as_str(), Some("filehost"));
        // debug only set in Cli (lowest here), but no other layer overrides it
        assert_eq!(get(&table, "debug").unwrap().as_bool(), Some(true));
    }

    #[cfg(feature = "url")]
    #[test]
    fn custom_order_url_highest_priority() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "5000".into())],
            env_prefix: Some("MYAPP".into()),
            url_overrides: vec![("port".into(), Value::Integer(7777))],
            cli_overrides: vec![("port".into(), Value::Integer(9999))],
            layer_order: Some(vec![Layer::Files, Layer::Env, Layer::Cli, Layer::Url]),
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        // Url is last → highest priority
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(7777));
    }

    // -- normalize_keys tests -------------------------------------------------

    #[test]
    fn normalize_off_kebab_file_key_rejected_by_strict() {
        // Baseline: without normalization, a kebab key in a config file is a
        // strict-mode violation. Locks the opt-in behavior.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-size = 25\n".into())],
            ..empty_input(&spec)
        };
        let result = resolve(input);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_on_kebab_file_key_accepted() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-size = 25\n".into())],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(25)
        );
    }

    #[test]
    fn normalize_on_snake_file_key_still_works() {
        // Backwards-compatible: snake-cased keys keep working when
        // normalization is on.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool_size = 30\n".into())],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(30)
        );
    }

    #[test]
    fn normalize_on_kebab_cli_override_accepted() {
        let spec = test_spec();
        let input = ResolveInput {
            cli_overrides: vec![("database.pool-size".into(), Value::Integer(77))],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(77)
        );
    }

    #[cfg(feature = "url")]
    #[test]
    fn normalize_on_kebab_url_override_accepted() {
        let spec = test_spec();
        let input = ResolveInput {
            url_overrides: vec![("database.pool-size".into(), Value::Integer(88))],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(88)
        );
    }

    #[test]
    fn normalize_on_kebab_typo_still_strict_errors() {
        // Normalization isn't a free pass — a kebab-cased *typo* still gets
        // flagged because the snake form is also unknown.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "[database]\npool-zize = 25\n".into())],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        // Reported in normalized form.
        assert_eq!(keys[0].key, "database.pool_zize");
    }

    #[test]
    fn normalize_on_collision_in_file_errors() {
        // Two distinct keys in the same table that normalize to the same
        // name must surface as an explicit error, not silently drop one.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "[database]\npool-size = 5\npool_size = 10\n".into(),
            )],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let result = resolve(input);
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
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![(
                "test.toml".into(),
                "host = \"x\"\n[database]\npool-size = 10\nurl = \"pg://y\"\n".into(),
            )],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let (table, _) = resolve(input).unwrap();
        assert_eq!(get(&table, "host").unwrap().as_str(), Some("x"));
        assert_eq!(
            get(&table, "database.pool_size").unwrap().as_integer(),
            Some(10)
        );
        assert_eq!(
            get(&table, "database.url").unwrap().as_str(),
            Some("pg://y")
        );
    }
}
