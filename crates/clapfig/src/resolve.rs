//! Core resolution pipeline: merge all config layers and produce a merged
//! config table.
//!
//! Operates on pre-loaded data (`ResolveInput`) with no I/O, making the full
//! pipeline testable with synthetic inputs (files, env, and the discovery
//! probe record). Steps:
//!
//! 1. Build each layer independently (files, env, URL, CLI)
//! 2. Merge layers in the configured order (default: files < env < URL < CLI)
//! 3. Finalize the merged table against the schema (defaults +
//!    required-field and type checks, via [`crate::schema_walk`])
//!
//! Each stage emits structured `tracing` events (target `"clapfig"`):
//! discovery probes, parse, layer construction, overlay wins (origins and
//! value types, never values), defaults filled, validation. `debug` is
//! per-stage summaries; `info` and above stay silent on a healthy load.
//!
//! The layer order is configurable via [`ResolveInput::layer_order`]. Both
//! entry points — `Clapfig::builder(schema)` and the derive-driven
//! `Clapfig::typed::<C>()` — thread in the same [`Schema`]; the typed path
//! deserializes the returned [`Map`] afterwards.

use std::path::PathBuf;
use std::sync::Arc;

use crate::env;
use crate::error::{ClapfigError, DiscoveryRecord};
use crate::format::{self, ConfigPath, FormatRegistry};
use crate::merge::deep_merge;
use crate::normalize::{normalize_key, normalize_table_and_spans};
use crate::origin::{Origin, OriginMap, origin_map_from_env, origin_map_from_file};
use crate::overrides;
use crate::runtime::Schema;
use crate::schema_walk;
use crate::strict::{CollectedUnknown, StrictnessOverrides, UnknownKeyHook};
use crate::types::Layer;
use crate::validate::{UnknownKeySource, ValidateContext, validate_unknown};
use crate::value::{Map, Value};

/// All pre-loaded data needed to resolve a config. No I/O happens here.
pub(crate) struct ResolveInput<'a> {
    /// The schema every layer is validated and finalized against.
    pub schema: &'a Schema,
    /// Enabled format adapters — the routing seam every file parse goes
    /// through. Per-file adapter selection is by extension; extensionless
    /// files (rc-style names) fall back to the preferred
    /// (first-registered) adapter, and an extension no enabled adapter
    /// claims is a hard [`ClapfigError::UnknownFormat`] — the same
    /// explicit-path rule as persist targets and `gen --output`, never a
    /// silent parse under another format.
    pub registry: &'a FormatRegistry,
    /// File contents in precedence order: first = lowest priority, last = highest.
    /// Loaded files only — misses and unprobed candidates live on
    /// [`discovery`](Self::discovery), not here.
    pub files: Vec<(PathBuf, String)>,
    /// Every candidate probe (loaded / missing / error / not probed) plus
    /// which non-file input types were consulted. Attached to
    /// [`ClapfigError::MissingRequired`]. Injectable so this walk stays
    /// I/O-free; production discovery fills it from the real search.
    pub discovery: DiscoveryRecord,
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
/// in the same shape as keys coming from normalized config files. The
/// original spelling is retained for origin facts.
fn normalize_override_keys(
    entries: &[(String, Value)],
    normalize_keys: bool,
) -> Vec<(String, String, Value)> {
    entries
        .iter()
        .map(|(k, v)| {
            let merge_key = if normalize_keys {
                normalize_key(k)
            } else {
                k.clone()
            };
            (merge_key, k.clone(), v.clone())
        })
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
/// (default: files < env < URL < CLI), then finalizes the merged table
/// against the schema. Returns the merged [`Map`] plus any keys the
/// `on_unknown_key` callback elected to
/// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect);
/// callers that don't need the collected list (the plain `load()`
/// surface) simply discard it via `let (out, _) = resolve(...)?;`.
pub(crate) fn resolve(
    input: ResolveInput<'_>,
) -> Result<(Map, Vec<CollectedUnknown>), ClapfigError> {
    // Build each layer independently, then merge in the configured order.

    let validate_ctx = ValidateContext {
        overrides: &input.strict_overrides,
        default_strict: input.strict_default,
        callback: input.unknown_key_hook.as_ref(),
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

    // Default order: Files < Env < Url < Cli. Resolved before layer
    // construction so omitting a layer excludes it entirely — including
    // unknown-key validation. Building the env table first used to
    // reject `APP__ROGUE` even when `Layer::Env` was not in the order;
    // the files table had the same hole (parse + unknown-key still ran
    // when `Layer::Files` was omitted).
    let default_order = default_layer_order();
    let order = input.layer_order.as_deref().unwrap_or(&default_order);

    crate::trace::discovery_complete(&input.discovery);

    // Files layer: parse → (optionally) normalize → validate → merge.
    // Validation runs against the parsed Table — never the raw text — so
    // normalized keys are checked in the same form they will reach the merge.
    // Origin trees are built after normalize so lookup keys match the
    // value tree; span bytes still point at the user's original spelling.
    let mut collected_unknowns: Vec<CollectedUnknown> = Vec::new();
    let (files_table, files_origins) = if order.contains(&Layer::Files) {
        let mut t = Map::new();
        let mut origins = OriginMap::new();
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
            let source: Arc<str> = Arc::from(content.as_str());
            let parsed = adapter
                .parse(content)
                .map_err(|e| ClapfigError::ParseError {
                    path: path.clone(),
                    source: Box::new(e),
                    source_text: Some(Arc::clone(&source)),
                })?;
            crate::trace::parsed_file(path, adapter.name());
            let mut table = match parsed.value {
                Value::Map(map) => map,
                other => {
                    let span = parsed.spans.get(&ConfigPath::new()).map(|e| e.value);
                    return Err(ClapfigError::InvalidValue {
                        key: path.display().to_string(),
                        reason: format!(
                            "config documents must be maps at the root, got {}",
                            other.type_str()
                        ),
                        origin: Box::new(
                            Origin::file_with_span(path.clone(), span, source).to_facts(),
                        ),
                    });
                }
            };
            let mut spans = parsed.spans;
            if input.normalize_keys {
                normalize_table_and_spans(&mut table, &mut spans)
                    .map_err(|c| c.into_error(path))?;
            }
            if cascade_active {
                let mut per_file = validate_unknown(
                    &table,
                    input.schema,
                    &UnknownKeySource::File {
                        path,
                        source: content,
                        spans: &spans,
                    },
                    &validate_ctx,
                )?;
                collected_unknowns.append(&mut per_file);
            }
            let file_origins = origin_map_from_file(&table, &spans, path, &source);
            (t, origins) = deep_merge(t, table, origins, file_origins);
        }
        crate::trace::files_layer_constructed(input.files.len(), t.len());
        (t, origins)
    } else {
        (Map::new(), OriginMap::new())
    };

    // Env layer. Sources travel with the table so unknown-key errors
    // name the exact variable that produced each path, not a
    // reconstructed uppercase spelling. Construction and validation
    // both gate on `Layer::Env` membership: an omitted env layer must
    // not fail (or fire `on_unknown_key`) for variables that will never
    // merge.
    let env_layer = if order.contains(&Layer::Env) {
        input
            .env_prefix
            .as_ref()
            .map(|prefix| env::env_to_table_with_sources(prefix, input.env_vars))
    } else {
        None
    };

    // Validate the env-derived table against the schema. Before this pass
    // env-unknown keys merged in unnoticed: `validate_unknown` only ran
    // per-file, the env layer landed afterwards, and `finalize` doesn't
    // re-check unknown keys. Now an env var like `MYAPP__ROGUE` flows
    // through the same cascade (and `on_unknown_key` callback) as a
    // file-supplied `rogue = 1` key, and a violation renders as an env
    // error naming the exact variable to unset.
    //
    // The walker is schema-driven (no typed deserialize) so type
    // mismatches between env's heuristic value parsing (e.g. string
    // "1.5" for an integer field) don't fail validation — that's
    // still the job of the final-merge type check inside `finalize`.
    if cascade_active && let Some((env_table_ref, sources, _)) = env_layer.as_ref() {
        let mut env_filtered = validate_unknown(
            env_table_ref,
            input.schema,
            &UnknownKeySource::Env { sources },
            &validate_ctx,
        )?;
        collected_unknowns.append(&mut env_filtered);
    }
    let env_layer = env_layer.map(|(table, _, winners)| {
        crate::trace::env_layer_constructed(table.len());
        let origins = origin_map_from_env(&table, &winners);
        (table, origins)
    });

    // URL layer
    #[cfg(feature = "url")]
    let url_layer = if input.url_overrides.is_empty() {
        None
    } else {
        let layer = overrides::overrides_to_table_with_original_keys(
            &normalize_override_keys(&input.url_overrides, input.normalize_keys),
            |k| Origin::url(k),
        );
        crate::trace::url_layer_constructed(layer.0.len());
        Some(layer)
    };

    // CLI layer
    let cli_layer = if input.cli_overrides.is_empty() {
        None
    } else {
        let layer = overrides::overrides_to_table_with_original_keys(
            &normalize_override_keys(&input.cli_overrides, input.normalize_keys),
            |k| Origin::r#override(k),
        );
        crate::trace::cli_layer_constructed(layer.0.len());
        Some(layer)
    };

    // Merge layers in the specified order (first = lowest priority)
    let mut merged = Map::new();
    let mut origins = OriginMap::new();
    for layer in order {
        let table_and_origins = match layer {
            Layer::Files => Some((files_table.clone(), files_origins.clone())),
            Layer::Env => env_layer.clone(),
            #[cfg(feature = "url")]
            Layer::Url => url_layer.clone(),
            Layer::Cli => cli_layer.clone(),
        };
        if let Some((t, layer_origins)) = table_and_origins {
            (merged, origins) = deep_merge(merged, t, origins, layer_origins);
        }
    }
    crate::trace::merge_complete(merged.len());

    // Schema-driven default injection: populate the table from the schema's
    // declared defaults so `finalize` only has to check required fields.
    // Default origins fill in the same walk (ADR-0004).
    schema_walk::fill_defaults_into(&mut merged, &mut origins, input.schema);

    let output = schema_walk::finalize(merged, &origins, input.schema, &input.discovery)?;
    crate::trace::validation_complete();
    Ok((output, collected_unknowns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;

    fn test_spec() -> Schema {
        test_schema()
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

    fn empty_input(schema: &Schema) -> ResolveInput<'_> {
        ResolveInput {
            schema,
            registry: toml_only_registry(),
            files: vec![],
            discovery: DiscoveryRecord::empty(),
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
    fn toml_unknown_key_in_array_of_tables_carets_the_right_line() {
        use crate::runtime::Field;
        let schema = Schema::object("App")
            .array_of(
                "servers",
                Schema::object("Server").field("host", Field::string().optional()),
            )
            .build();
        let source = "[[servers]]\nhost = \"a\"\n[[servers]]\nrogue = 1\n";
        let input = ResolveInput {
            files: vec![("config.toml".into(), source.into())],
            ..empty_input(&schema)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "servers[1].rogue");
        assert_eq!(keys[0].line, 4);
        let span = keys[0].span.expect("key span");
        assert_eq!(&source[span.start..span.end], "rogue");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("rogue = 1"), "{out}");
        assert!(out.contains(":4"), "{out}");
    }

    #[test]
    fn toml_unknown_under_quoted_dotted_map_of_key_carets_the_key() {
        use crate::runtime::Field;
        let schema = Schema::object("App")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("host", Field::string().optional()),
            )
            .build();
        let source = "[plugins.\"acme.prod\"]\nrogue = 1\n";
        let input = ResolveInput {
            files: vec![("config.toml".into(), source.into())],
            ..empty_input(&schema)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 2);
        let span = keys[0].span.expect("key span");
        assert_eq!(&source[span.start..span.end], "rogue");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("rogue = 1"), "{out}");
        assert!(out.contains(":2"), "{out}");
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
        // The violation is reported as an env problem naming the exact
        // variable to unset, never dressed as a config-file error.
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__ROGUE_KEY"));
        let msg = err.to_string();
        assert!(msg.contains("MYAPP__ROGUE_KEY"), "{msg}");
        assert!(!msg.contains("config file"), "{msg}");
    }

    #[test]
    fn env_unknown_key_names_the_mixed_case_variable() {
        // The suffix is accepted in any case and lowercased for the
        // table path. The error must name the spelling that is actually
        // set — unsetting a reconstructed MYAPP__ROGUE_KEY would leave
        // MYAPP__rogue_key in place on a case-sensitive platform.
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__rogue_key".into(), "1".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "rogue_key");
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__rogue_key"));
        let msg = err.to_string();
        assert!(msg.contains("MYAPP__rogue_key"), "{msg}");
        assert!(!msg.contains("MYAPP__ROGUE_KEY"), "{msg}");
    }

    #[test]
    fn env_unknown_nested_section_names_the_variable() {
        // No `database` field: the walker reports `database`, not
        // `database.rogue`. The original variable must still be named.
        use crate::runtime::{Field, Schema};
        let spec = Schema::object("App")
            .field("host", Field::string().default("localhost"))
            .build();
        let input = ResolveInput {
            env_vars: vec![("MYAPP__DATABASE__ROGUE".into(), "1".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "database");
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__DATABASE__ROGUE"));
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
                    assert_eq!(ctx.env_var, Some("MYAPP__ROGUE_KEY"));
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
    fn omitted_files_layer_ignores_unknown_file_keys() {
        // Omitting Layer::Files excludes parse/unknown-key validation
        // on supplied file contents — same "omit a layer to exclude it"
        // rule as Env.
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\nrogue = 1\n".into())],
            layer_order: Some(vec![Layer::Cli]),
            cli_overrides: vec![("port".into(), Value::Integer(7777))],
            ..empty_input(&spec)
        };
        let (table, collected) =
            resolve(input).expect("omitted files must not fail on unknown file keys");
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(7777));
        assert!(collected.is_empty());
        assert!(get(&table, "rogue").is_none());
    }

    #[test]
    fn omitted_env_layer_ignores_unknown_env_keys() {
        // Omitting Layer::Env excludes the layer entirely: an unknown
        // env key must not fail unknown-key validation or invoke the
        // callback. The documented "omit a layer to exclude it" rule
        // covers validation, not just merge.
        let saw_env_unknown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw = std::sync::Arc::clone(&saw_env_unknown);
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__ROGUE_KEY".into(), "1".into())],
            env_prefix: Some("MYAPP".into()),
            layer_order: Some(vec![Layer::Files, Layer::Cli]),
            unknown_key_hook: Some(std::sync::Arc::new(move |ctx| {
                if ctx.path == "rogue_key" {
                    saw.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                crate::strict::UnknownKeyDecision::Reject
            })),
            ..empty_input(&spec)
        };
        let (table, collected) = resolve(input).expect("omitted env must not fail on APP__ROGUE");
        assert_eq!(get(&table, "port").unwrap().as_integer(), Some(3000));
        assert!(collected.is_empty());
        assert!(
            !saw_env_unknown.load(std::sync::atomic::Ordering::SeqCst),
            "on_unknown_key must not run for env keys when Layer::Env is omitted"
        );
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

    fn required_name_schema() -> Schema {
        Schema::object("Req")
            .field("name", crate::runtime::Field::string())
            .build()
    }

    #[test]
    fn missing_required_uses_injected_discovery_record() {
        // The core walk is I/O-free: probes on ResolveInput are the
        // record MissingRequired reports, including FirstMatch's
        // not-probed candidates. No filesystem is touched.
        let schema = required_name_schema();
        let discovery = DiscoveryRecord {
            files: vec![
                crate::error::FileProbe {
                    path: "/etc/app.toml".into(),
                    outcome: crate::error::ProbeOutcome::Missing,
                },
                crate::error::FileProbe {
                    path: "/proj/app.toml".into(),
                    outcome: crate::error::ProbeOutcome::NotProbed,
                },
            ],
            env: true,
            url: false,
            overrides: true,
        };
        let input = ResolveInput {
            discovery: discovery.clone(),
            ..empty_input(&schema)
        };
        let err = resolve(input).unwrap_err();
        match err {
            ClapfigError::MissingRequired {
                key,
                discovery: got,
            } => {
                assert_eq!(key, "name");
                assert_eq!(got, discovery);
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn nested_missing_leaf_is_missing_required_with_injected_discovery() {
        // Parent map present from a file; required leaf absent. Same
        // diagnostic as a top-level miss — no nearest-ancestor origin.
        let schema = Schema::object("Req")
            .nested(
                "db",
                Schema::object("Db").field("url", crate::runtime::Field::string()),
            )
            .build();
        let discovery = DiscoveryRecord {
            files: vec![crate::error::FileProbe {
                path: "app.toml".into(),
                outcome: crate::error::ProbeOutcome::Loaded,
            }],
            env: false,
            url: false,
            overrides: false,
        };
        let input = ResolveInput {
            files: vec![("app.toml".into(), "[db]\n".into())],
            discovery: discovery.clone(),
            ..empty_input(&schema)
        };
        let err = resolve(input).unwrap_err();
        let msg = err.to_string();
        match err {
            ClapfigError::MissingRequired {
                key,
                discovery: got,
            } => {
                assert_eq!(key, "db.url");
                assert_eq!(got, discovery);
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
        assert!(
            !msg.contains("set by"),
            "MissingRequired must not name a winning origin: {msg}"
        );
    }

    fn assert_invalid_value<'a>(
        err: &'a ClapfigError,
        key: &str,
        input_type: crate::types::InputType,
    ) -> &'a crate::error::OriginFacts {
        match err {
            ClapfigError::InvalidValue {
                key: got_key,
                origin,
                ..
            } => {
                assert_eq!(got_key, key);
                assert_eq!(origin.input_type, Some(input_type));
                origin.as_ref()
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn invalid_value_file_names_path_line_and_carets_the_value() {
        let spec = test_spec();
        let source = "port = \"not-a-number\"\n";
        let input = ResolveInput {
            files: vec![("test.toml".into(), source.into())],
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "port", crate::types::InputType::File);
        assert_eq!(
            facts.file.as_deref(),
            Some(std::path::Path::new("test.toml"))
        );
        let span = facts.span.expect("value span");
        assert_eq!(&source[span.start..span.end], "\"not-a-number\"");
        let msg = err.to_string();
        assert!(msg.contains("test.toml:1"), "{msg}");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("port = \"not-a-number\""), "{out}");
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        let caret_run = caret.chars().filter(|&c| c == '^').count();
        assert_eq!(
            caret_run,
            source[span.start..span.end].chars().count(),
            "caret should cover the value span, got: {out}"
        );
    }

    #[test]
    fn invalid_value_env_names_the_variable() {
        let spec = test_spec();
        let input = ResolveInput {
            files: vec![("test.toml".into(), "port = 3000\n".into())],
            env_vars: vec![("MYAPP__PORT".into(), "not-a-number".into())],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "port", crate::types::InputType::Env);
        assert_eq!(facts.env_var.as_deref(), Some("MYAPP__PORT"));
        let msg = err.to_string();
        assert!(
            msg.contains("set by environment variable MYAPP__PORT"),
            "{msg}"
        );
    }

    #[test]
    fn invalid_value_env_names_only_the_winning_variable() {
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![
                ("MYAPP__DATABASE__URL".into(), "postgres://ok".into()),
                ("MYAPP__DATABASE".into(), "oops".into()),
            ],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "database", crate::types::InputType::Env);
        assert_eq!(facts.env_var.as_deref(), Some("MYAPP__DATABASE"));
        let msg = err.to_string();
        assert!(msg.contains("MYAPP__DATABASE"), "{msg}");
        assert!(
            !msg.contains("MYAPP__DATABASE__URL"),
            "losing nested var must not appear on the origin: {msg}"
        );
    }

    #[test]
    fn invalid_value_env_nested_replaces_flat_origin() {
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![
                ("MYAPP__DATABASE".into(), "oops".into()),
                ("MYAPP__DATABASE__POOL_SIZE".into(), "nope".into()),
            ],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "database.pool_size", crate::types::InputType::Env);
        assert_eq!(facts.env_var.as_deref(), Some("MYAPP__DATABASE__POOL_SIZE"));
    }

    #[test]
    fn invalid_value_env_case_collision_names_last_writer() {
        let spec = test_spec();
        let input = ResolveInput {
            env_vars: vec![
                ("MYAPP__port".into(), "first".into()),
                ("MYAPP__PORT".into(), "second".into()),
            ],
            env_prefix: Some("MYAPP".into()),
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "port", crate::types::InputType::Env);
        assert_eq!(facts.env_var.as_deref(), Some("MYAPP__PORT"));
        assert!(
            !err.to_string().contains("MYAPP__port"),
            "losing case variant must not appear on the origin: {}",
            err
        );
    }

    #[cfg(feature = "url")]
    #[test]
    fn invalid_value_url_names_the_query_key() {
        let spec = test_spec();
        let input = ResolveInput {
            url_overrides: vec![("database.pool_size".into(), Value::String("big".into()))],
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "database.pool_size", crate::types::InputType::Url);
        assert_eq!(facts.url_key.as_deref(), Some("database.pool_size"));
        let msg = err.to_string();
        assert!(
            msg.contains("set by URL query parameter database.pool_size"),
            "{msg}"
        );
    }

    #[cfg(feature = "url")]
    #[test]
    fn invalid_value_url_keeps_percent_decoded_dotted_key() {
        let spec = test_spec();
        let pairs = crate::url::query_to_overrides("database.pool_size=big");
        let input = ResolveInput {
            url_overrides: pairs,
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "database.pool_size", crate::types::InputType::Url);
        assert_eq!(facts.url_key.as_deref(), Some("database.pool_size"));
    }

    #[test]
    fn invalid_value_override_names_the_override_key() {
        let spec = test_spec();
        let input = ResolveInput {
            cli_overrides: vec![("port".into(), Value::String("nope".into()))],
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "port", crate::types::InputType::Override);
        assert_eq!(facts.key.as_deref(), Some("port"));
        let msg = err.to_string();
        assert!(
            msg.contains("set by a programmatic override for key port"),
            "{msg}"
        );
    }

    #[test]
    fn invalid_value_default_names_the_schema_key() {
        // Runtime schema whose declared default fails the leaf's enum check.
        let schema = Schema::object("App")
            .field(
                "level",
                crate::runtime::Field::enum_of(["debug", "info"]).default("verbose"),
            )
            .build();
        let err = resolve(empty_input(&schema)).unwrap_err();
        let facts = assert_invalid_value(&err, "level", crate::types::InputType::Default);
        assert_eq!(facts.key.as_deref(), Some("level"));
        let msg = err.to_string();
        assert!(msg.contains("set by schema default for key level"), "{msg}");
    }

    #[test]
    fn normalize_keys_origin_lookup_shows_user_spelling_in_snippet() {
        let spec = test_spec();
        let source = "[database]\npool-size = \"oops\"\n";
        let input = ResolveInput {
            files: vec![("test.toml".into(), source.into())],
            normalize_keys: true,
            ..empty_input(&spec)
        };
        let err = resolve(input).unwrap_err();
        let facts = assert_invalid_value(&err, "database.pool_size", crate::types::InputType::File);
        let span = facts.span.expect("value span");
        assert_eq!(&source[span.start..span.end], "\"oops\"");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("pool-size = \"oops\""), "{out}");
        assert!(!out.contains("pool_size ="), "{out}");
    }

    #[test]
    fn quoted_dotted_key_origin_is_not_nested_path() {
        // Schema field names cannot contain `.` (dotted-path separator).
        // A MapOf entry key `"a.b"` is one ConfigPath segment; nested
        // `[a] b` is two. Origin lookup must not confuse them.
        let quoted_schema = Schema::object("App")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("host", crate::runtime::Field::integer()),
            )
            .build();
        let quoted = "[plugins.\"a.b\"]\nhost = \"oops\"\n";
        let err = resolve(ResolveInput {
            files: vec![("quoted.toml".into(), quoted.into())],
            ..empty_input(&quoted_schema)
        })
        .unwrap_err();
        let facts = assert_invalid_value(&err, "plugins.a.b.host", crate::types::InputType::File);
        let span = facts.span.expect("value span");
        assert_eq!(&quoted[span.start..span.end], "\"oops\"");
        // The span lives on the quoted-key assignment, not a nested [a] b path.
        assert!(quoted[..span.start].contains("\"a.b\""), "{quoted}");

        let nested_schema = Schema::object("App")
            .nested(
                "a",
                Schema::object("A").field("b", crate::runtime::Field::integer()),
            )
            .build();
        let nested = "[a]\nb = \"oops\"\n";
        let err = resolve(ResolveInput {
            files: vec![("nested.toml".into(), nested.into())],
            ..empty_input(&nested_schema)
        })
        .unwrap_err();
        let facts = assert_invalid_value(&err, "a.b", crate::types::InputType::File);
        let span = facts.span.expect("value span");
        assert_eq!(&nested[span.start..span.end], "\"oops\"");
        assert!(nested[..span.start].contains("[a]"), "{nested}");
    }

    #[test]
    fn post_validation_failed_has_no_origin_line() {
        let err = ClapfigError::PostValidationFailed("port too low".into());
        let msg = err.to_string();
        assert_eq!(msg, "Configuration validation failed: port too low");
        assert!(!msg.contains("set by"), "{msg}");
        assert!(!msg.contains("-->"), "{msg}");
    }
}
