//! Strict-mode validation: detect unknown keys in config files.
//!
//! Operates on an already-parsed value [`Map`] so it sees exactly the same
//! keys that will reach the merge step. When kebab-case normalization is
//! enabled the table arrives with `-` already rewritten to `_`, and the
//! file's span index is rewritten with the same walk so unknown-key errors
//! still caret the user's original spelling.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::{ClapfigError, UnknownKeyInfo};
use crate::format::{ConfigPath, PathSegment, Span, SpanEntry, byte_offset_to_line_col};
use crate::origin::OriginMap;
use crate::runtime::{DocumentRoot, Schema};
use crate::strict::{
    CollectedUnknown, StrictnessOverrides, UnknownKeyContext, UnknownKeyDecision, UnknownKeyHook,
};
use crate::types::InputType;
use crate::value::{Map, Value};

/// Per-resolution strictness configuration passed into the validate path.
///
/// Bundles the cascade overrides, the builder-level default ([Knob 1]),
/// and the optional [`on_unknown_key`](crate::Builder::on_unknown_key)
/// callback. When kebab-case normalization is on, the caller rewrites
/// the table and span-index paths before this walk; line/caret come
/// from the key span, which still points at the original spelling.
///
/// [Knob 1]: crate::Builder::strict
pub(crate) struct ValidateContext<'a> {
    pub overrides: &'a StrictnessOverrides,
    pub default_strict: bool,
    pub callback: Option<&'a UnknownKeyHook>,
}

/// Where the table under validation came from — decides how an
/// unknown-key violation is reported.
///
/// A `File` source carries the raw text (for renderer snippets), the
/// file path, and that file's span index (key span for the caret). An
/// `Env` source carries the original variable name(s) for each
/// structured path so each violation can name the exact variable to unset
/// (`MYAPP__rogue_key`, not a reconstructed `MYAPP__ROGUE_KEY`) instead
/// of wearing config-file clothing.
pub(crate) enum UnknownKeySource<'a> {
    File {
        path: &'a Path,
        source: &'a str,
        spans: &'a BTreeMap<ConfigPath, SpanEntry>,
    },
    Env {
        sources: &'a crate::env::EnvSources,
    },
    /// Post-merge branch-exclusive keys: locations come from the merged
    /// origin tree (winner-only), not a single layer's spans.
    Merged {
        origins: &'a OriginMap,
    },
}

/// Single unknown-key entry passed to `filter_through_cascade`.
///
/// `path` is the dotted display form (cascade lookup, error rendering,
/// `on_unknown_key`). `leaf` is the raw map key at the leaf — distinct
/// from the trailing dot-split segment when that key was quoted with
/// `.` inside it (a literal `"acme.task-due-date-missing"`).
/// `config_path` is the structured address the parser indexed; span
/// lookup and value lookup use it rather than reconstructing from
/// `path`, so a quoted dotted ancestor (`[plugins."acme.prod"]`) stays
/// one segment.
pub(crate) struct UnknownKey {
    pub path: String,
    pub leaf: String,
    pub config_path: ConfigPath,
}

/// Detect unknown keys in a parsed config table: walk `table` against
/// `schema` ([`crate::schema_walk::collect_unknown_paths`]), then resolve
/// every hit through the strictness cascade and the optional
/// `on_unknown_key` callback ([`filter_through_cascade`]).
///
/// One entry point serves every layer. The per-file pass supplies an
/// [`UnknownKeySource::File`] (source text, path, and that file's span
/// index); the env-layer pass supplies [`UnknownKeySource::Env`] with
/// the original variable names — env vars are merged after the per-file
/// pass, so without this call an `MYAPP__ROGUE_KEY=...` would slip into
/// the merged result without ever reaching the cascade or the
/// `on_unknown_key` callback, and violations render as env errors naming
/// the exact variable that produced the key.
///
/// Returns the keys the callback opted to
/// [`UnknownKeyDecision::Collect`] — empty for callers that don't use
/// the collect path. Reject decisions surface as
/// `ClapfigError::UnknownKeys`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_unknown(
    table: &Map,
    schema: &Schema,
    origin: &UnknownKeySource<'_>,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError> {
    validate_unknown_root(table, DocumentRoot::Object(schema), origin, ctx)
}

/// Same as [`validate_unknown`], walking a document root (object or tagged).
pub(crate) fn validate_unknown_root(
    table: &Map,
    root: DocumentRoot<'_>,
    origin: &UnknownKeySource<'_>,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError> {
    let mut unknown: Vec<UnknownKey> = Vec::new();
    crate::schema_walk::collect_unknown_paths_root(
        table,
        root,
        "",
        &ConfigPath::new(),
        &mut unknown,
    );
    filter_through_cascade(table, origin, unknown, ctx)
}

/// Unknown-key walk against a document-root [`Shape`].
///
/// Object roots use [`validate_unknown`]. Map roots collect unknown keys
/// inside entries (root keys are user data, not unknown).
pub(crate) fn validate_unknown_shape(
    table: &Map,
    shape: &crate::runtime::Shape,
    origin: &UnknownKeySource<'_>,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError> {
    match shape {
        crate::runtime::Shape::Object(schema) => validate_unknown(table, schema, origin, ctx),
        _ => {
            let mut unknown: Vec<UnknownKey> = Vec::new();
            crate::schema_walk::collect_unknown_against_root(table, shape, &mut unknown);
            filter_through_cascade(table, origin, unknown, ctx)
        }
    }
}

/// Resolve an already-collected list of unknown paths against the cascade
/// and the optional `on_unknown_key` callback. Shared between the per-file
/// walker and the env-layer walker so both have identical strictness
/// semantics.
///
/// Decision chain (per the proposal):
///
/// 1. If the cascade says **lenient**, drop silently. Done.
/// 2. If the cascade says **strict** and a callback is registered, call it
///    — `Accept` drops silently; `Reject` produces an `UnknownKeys` entry;
///    `Collect` appends a [`CollectedUnknown`] to the returned list and
///    keeps loading.
/// 3. If no callback, the cascade decision stands (reject).
///
/// Returns the keys collected via [`UnknownKeyDecision::Collect`]. Empty
/// when no callback is registered, no key opts in, or every unknown key
/// fell through to a Reject decision (in which case the error path runs
/// instead).
pub(crate) fn filter_through_cascade(
    table: &Map,
    origin: &UnknownKeySource<'_>,
    unknown_keys: Vec<UnknownKey>,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError> {
    if unknown_keys.is_empty() {
        return Ok(Vec::new());
    }
    let source_arc: Option<Arc<str>> = match origin {
        UnknownKeySource::File { source, .. } => Some(Arc::from(*source)),
        UnknownKeySource::Env { .. } | UnknownKeySource::Merged { .. } => None,
    };
    let mut rejected: Vec<UnknownKeyInfo> = Vec::new();
    let mut collected: Vec<CollectedUnknown> = Vec::new();
    for entry in unknown_keys {
        let UnknownKey {
            path: key,
            leaf,
            config_path,
        } = entry;
        let strict = ctx
            .overrides
            .effective_strict(&key, &leaf, ctx.default_strict);
        if !strict {
            // Lenient subtree — drop silently.
            continue;
        }

        // File identity and the key span live on the file source;
        // env-derived keys instead carry the variable name so errors
        // name the thing to unset. Missing span-index lookup omits the
        // line rather than inventing a heuristic. Normalization, when
        // enabled, already rewrote the table and span-index paths
        // before this walk; the key span still points at the original
        // spelling. Merged (phase 2) locations come from the origin tree.
        let located = locate_unknown_key(origin, &config_path, source_arc.as_ref());
        let value_ref = lookup_value(table, &config_path);

        if let Some(callback) = ctx.callback {
            // Callback runs only on cascade-strict keys. Look the value
            // up by the structured path so quoted keys containing dots
            // (literal `"acme.prod"` ancestors, `"a.b"` leaves) and
            // `[N]` array indexes resolve to the real entry. `value` is
            // `None` when the lookup genuinely can't resolve
            // (out-of-bounds index, path through a non-table) — the
            // callback still runs and can decide based on
            // path/leaf/file/line alone. Env-derived keys arrive with
            // `file: None`.
            let context = UnknownKeyContext {
                path: &key,
                leaf: &leaf,
                value: value_ref,
                file: located.file,
                line: if located.line > 0 {
                    Some(located.line)
                } else {
                    None
                },
                span: located.span,
                env_var: located.env_var.as_deref(),
                url_key: located.url_key.as_deref(),
                override_key: located.override_key.as_deref(),
                input_type: located.input_type,
            };
            match callback(&context) {
                UnknownKeyDecision::Accept => continue,
                UnknownKeyDecision::Collect => {
                    collected.push(CollectedUnknown {
                        path: key,
                        leaf,
                        value: value_ref.cloned(),
                        file: located.file.map(Path::to_path_buf),
                        line: if located.line > 0 {
                            Some(located.line)
                        } else {
                            None
                        },
                        span: located.span,
                        env_var: located.env_var,
                        url_key: located.url_key,
                        override_key: located.override_key,
                        input_type: located.input_type,
                    });
                    continue;
                }
                UnknownKeyDecision::Reject => { /* fall through to reject */ }
            }
        }

        rejected.push(UnknownKeyInfo {
            key,
            path: unknown_key_source_path(&located),
            line: located.line,
            source: located.source,
            env_var: located.env_var,
            span: located.span,
            url_key: located.url_key,
            override_key: located.override_key,
            input_type: located.input_type,
        });
    }
    if rejected.is_empty() {
        Ok(collected)
    } else {
        Err(ClapfigError::UnknownKeys(rejected))
    }
}

struct LocatedUnknown<'a> {
    file: Option<&'a Path>,
    line: usize,
    env_var: Option<String>,
    span: Option<Span>,
    url_key: Option<String>,
    override_key: Option<String>,
    input_type: Option<InputType>,
    source: Option<Arc<str>>,
}

fn unknown_key_source_path(located: &LocatedUnknown<'_>) -> std::path::PathBuf {
    if let Some(file) = located.file {
        return file.to_path_buf();
    }
    match located.input_type {
        Some(InputType::Url) => std::path::PathBuf::from("<url>"),
        Some(InputType::Override) => std::path::PathBuf::from("<override>"),
        _ => std::path::PathBuf::from("<env>"),
    }
}

fn locate_unknown_key<'a>(
    origin: &'a UnknownKeySource<'_>,
    config_path: &ConfigPath,
    file_source: Option<&'a Arc<str>>,
) -> LocatedUnknown<'a> {
    match origin {
        UnknownKeySource::File {
            path,
            source,
            spans,
        } => {
            let key_span = spans.get(config_path).and_then(|e| e.key);
            let line = key_span
                .map(|s| byte_offset_to_line_col(source, s.start).0)
                .unwrap_or(0);
            LocatedUnknown {
                file: Some(*path),
                line,
                env_var: None,
                span: key_span,
                url_key: None,
                override_key: None,
                input_type: None,
                source: file_source.cloned(),
            }
        }
        UnknownKeySource::Env { sources } => LocatedUnknown {
            file: None,
            line: 0,
            env_var: crate::env::env_source_names(sources, config_path),
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
            source: None,
        },
        UnknownKeySource::Merged { origins } => {
            let Some(found) = crate::origin::lookup(origins, config_path) else {
                return LocatedUnknown {
                    file: None,
                    line: 0,
                    env_var: None,
                    span: None,
                    url_key: None,
                    override_key: None,
                    input_type: None,
                    source: None,
                };
            };
            let key_span = found.key_span;
            let line = match (key_span, found.source.as_deref()) {
                (Some(span), Some(src)) => byte_offset_to_line_col(src, span.start).0,
                _ => 0,
            };
            LocatedUnknown {
                file: found.file.as_deref(),
                line,
                env_var: if found.env_vars.is_empty() {
                    None
                } else {
                    Some(found.env_vars.join(", "))
                },
                span: key_span,
                url_key: found.url_key.clone(),
                override_key: if found.layer == InputType::Override {
                    found.key.clone()
                } else {
                    None
                },
                input_type: Some(found.layer),
                source: found.source.clone(),
            }
        }
    }
}

/// Look up a value in a parsed table by the structured path the walker
/// recorded (the same [`ConfigPath`] the adapter indexed).
///
/// Display of a [`ConfigPath`] is one-way; this walks the segments, so a
/// quoted dotted ancestor (`plugins` → `acme.prod` → `rogue`) and an
/// array index (`plugins` → `[1]` → `rogue`) both resolve. Returns
/// `None` when the lookup genuinely fails: a non-table intermediate, a
/// missing key, or an out-of-bounds array index. The callback receives
/// this `Option` through
/// [`UnknownKeyContext::value`](crate::UnknownKeyContext::value).
fn lookup_value<'a>(table: &'a Map, path: &ConfigPath) -> Option<&'a Value> {
    enum Cursor<'a> {
        Map(&'a Map),
        Array(&'a [Value]),
    }
    let mut current = Cursor::Map(table);
    let segs = path.segments();
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i + 1 == segs.len();
        current = match (current, seg) {
            (Cursor::Map(m), PathSegment::Key(k)) => {
                let v = m.get(k)?;
                if is_last {
                    return Some(v);
                }
                match v {
                    Value::Map(next) => Cursor::Map(next),
                    Value::Array(next) => Cursor::Array(next),
                    _ => return None,
                }
            }
            (Cursor::Array(a), PathSegment::Index(idx)) => {
                let v = a.get(*idx)?;
                if is_last {
                    return Some(v);
                }
                match v {
                    Value::Map(next) => Cursor::Map(next),
                    Value::Array(next) => Cursor::Array(next),
                    _ => return None,
                }
            }
            _ => return None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FormatAdapter;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn path() -> PathBuf {
        PathBuf::from("/test/config.toml")
    }

    /// Run unknown-key validation against the shared test schema, the way
    /// the resolve pipeline does per file: parse (TOML spans filled),
    /// optionally normalize table + span paths, then validate.
    fn validate(
        content: &str,
        path: &Path,
        ctx: &ValidateContext<'_>,
        normalize_keys: bool,
    ) -> Result<Vec<CollectedUnknown>, ClapfigError> {
        let parsed = crate::format::TomlAdapter
            .parse(content)
            .expect("test fixture TOML must parse");
        let mut table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => unreachable!("TOML documents are maps at the root"),
        };
        let mut spans = parsed.spans;
        if normalize_keys {
            crate::normalize::normalize_table_and_spans(&mut table, &mut spans)
                .expect("test fixtures must not contain collisions");
        }
        validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::File {
                path,
                source: content,
                spans: &spans,
            },
            ctx,
        )
    }

    /// Default validate context: strict on, no overrides, no callback.
    /// Mirrors the pre-Phase-3 default and is the right baseline for every
    /// existing test in this module.
    fn test_ctx() -> ValidateContext<'static> {
        static EMPTY: OnceLock<StrictnessOverrides> = OnceLock::new();
        let overrides = EMPTY.get_or_init(StrictnessOverrides::new);
        ValidateContext {
            overrides,
            default_strict: true,
            callback: None,
        }
    }

    #[test]
    fn env_origin_reports_variable_name_not_file() {
        // An env-derived unknown key is reported as an env problem: the
        // original variable name is taken from the source map (not
        // reconstructed), no source text or line is attached, and no
        // file path leaks in.
        let mut table = Map::new();
        let mut db = Map::new();
        db.insert("rogue".into(), crate::value::Value::Integer(1));
        table.insert("database".into(), crate::value::Value::Map(db));
        let mut sources = crate::env::EnvSources::new();
        sources.insert(
            ConfigPath::new().key("database").key("rogue"),
            vec!["MYAPP__DATABASE__ROGUE".into()],
        );
        let err = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "database.rogue");
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__DATABASE__ROGUE"));
        assert_eq!(keys[0].line, 0);
        assert!(keys[0].source.is_none());
    }

    #[test]
    fn env_origin_names_the_mixed_case_variable_that_produced_the_value() {
        // Reconstructing an uppercased path would tell the user to
        // unset MYAPP__ROGUE_KEY, which does not remove MYAPP__rogue_key
        // on a case-sensitive platform.
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [("MYAPP__rogue_key".into(), "1".into())],
        );
        let err = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "rogue_key");
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__rogue_key"));
    }

    #[test]
    fn env_origin_lists_every_source_name_that_collapsed_onto_the_path() {
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [
                ("MYAPP__rogue_key".into(), "1".into()),
                ("MYAPP__ROGUE_KEY".into(), "2".into()),
            ],
        );
        let err = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(
            keys[0].env_var.as_deref(),
            Some("MYAPP__rogue_key, MYAPP__ROGUE_KEY")
        );
    }

    /// Schema with no `database` section — unknown-key traversal stops
    /// at that ancestor instead of walking to `database.rogue`.
    fn schema_without_database() -> Schema {
        use crate::runtime::{Field, Schema};
        Schema::object("App")
            .field("host", Field::string().default("localhost"))
            .build()
    }

    #[test]
    fn env_origin_names_variable_when_unknown_is_a_nested_section() {
        // MYAPP__DATABASE__ROGUE is stored under `database.rogue`, but
        // with no `database` field the walker reports `database`.
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [("MYAPP__DATABASE__ROGUE".into(), "1".into())],
        );
        let err = validate_unknown(
            &table,
            &schema_without_database(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "database");
        assert_eq!(keys[0].env_var.as_deref(), Some("MYAPP__DATABASE__ROGUE"));
    }

    #[test]
    fn env_origin_lists_flat_and_nested_names_after_nested_overwrite() {
        // Flat then nested: the table is the nested section, but both
        // original names touched this path and must be listed.
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [
                ("MYAPP__DATABASE".into(), "flat".into()),
                ("MYAPP__DATABASE__ROGUE".into(), "1".into()),
            ],
        );
        assert!(table["database"].as_map().is_some());
        let err = validate_unknown(
            &table,
            &schema_without_database(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "database");
        assert_eq!(
            keys[0].env_var.as_deref(),
            Some("MYAPP__DATABASE, MYAPP__DATABASE__ROGUE")
        );
    }

    #[test]
    fn env_origin_lists_flat_and_nested_names_after_flat_overwrite() {
        // Nested then flat: the table holds the flat value, but the
        // nested variable is still set in the environment.
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [
                ("MYAPP__DATABASE__ROGUE".into(), "1".into()),
                ("MYAPP__DATABASE".into(), "flat".into()),
            ],
        );
        assert_eq!(table["database"].as_str(), Some("flat"));
        let err = validate_unknown(
            &table,
            &schema_without_database(),
            &UnknownKeySource::Env { sources: &sources },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "database");
        assert_eq!(
            keys[0].env_var.as_deref(),
            Some("MYAPP__DATABASE, MYAPP__DATABASE__ROGUE")
        );
    }

    #[test]
    fn env_origin_passes_variable_name_to_callback_and_collect() {
        // `env_var` is already computed for UnknownKeyInfo; the callback
        // context and collected-unknown list must see the same name.
        let (table, sources, _) = crate::env::env_to_table_with_sources(
            "MYAPP",
            [("MYAPP__rogue_key".into(), "1".into())],
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<Option<String>>));
        let seen_cb = std::sync::Arc::clone(&seen);
        let hook: UnknownKeyHook = Arc::new(move |ctx| {
            *seen_cb.lock().unwrap() = Some(ctx.env_var.map(str::to_owned));
            UnknownKeyDecision::Collect
        });
        let ctx = ValidateContext {
            overrides: test_ctx().overrides,
            default_strict: true,
            callback: Some(&hook),
        };
        let collected = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::Env { sources: &sources },
            &ctx,
        )
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some(Some("MYAPP__rogue_key".into()))
        );
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].env_var.as_deref(), Some("MYAPP__rogue_key"));
    }

    #[test]
    fn valid_config_passes() {
        let content = r#"
host = "0.0.0.0"
port = 3000
debug = true

[database]
url = "postgres://localhost"
pool_size = 10
"#;
        let result = validate(content, &path(), &test_ctx(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn unknown_top_level_key() {
        let content = "host = \"localhost\"\ntypo_key = 42\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "typo_key");
        assert_eq!(keys[0].line, 2);
        assert!(keys[0].source.is_some());
    }

    #[test]
    fn unknown_nested_key() {
        let content = "[database]\nurl = \"pg://\"\ntypo = \"bad\"\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "database.typo");
        assert_eq!(keys[0].leaf(), "typo");
    }

    #[test]
    fn multiple_unknown_keys() {
        let content = "typo1 = 1\ntypo2 = 2\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn line_number_accuracy() {
        let content = "host = \"x\"\nport = 8080\ndebug = false\n\n# comment\nbad_key = 1\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 6);
    }

    #[test]
    fn empty_content_ok() {
        let result = validate("", &path(), &test_ctx(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn known_optional_field_ok() {
        let content = "[database]\nurl = \"pg://\"\n";
        let result = validate(content, &path(), &test_ctx(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn sparse_config_ok() {
        let content = "port = 3000\n";
        let result = validate(content, &path(), &test_ctx(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn error_includes_file_path() {
        let content = "typo = 1\n";
        let p = PathBuf::from("/home/user/.config/myapp/config.toml");
        let err = validate(content, &p, &test_ctx(), false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("config.toml") || msg.contains("Unknown keys"));
    }

    #[test]
    fn line_number_finds_correct_section_for_duplicate_leaf() {
        let content = "host = \"x\"\nport = 8080\n[database]\ntypo = \"bad\"\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "database.typo");
        assert_eq!(keys[0].line, 4);
    }

    #[test]
    fn line_number_top_level_not_confused_by_nested_same_name() {
        let content = "typo = 99\n[database]\npool_size = 5\n";
        let result = validate(content, &path(), &test_ctx(), false);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "typo");
        assert_eq!(keys[0].line, 1);
    }

    // -- normalize_keys = true ------------------------------------------------

    #[test]
    fn normalize_kebab_top_level_key_is_valid() {
        // `pool_size` isn't a top-level field but `host` is — exercise the
        // happy path where a kebab key normalizes to a known snake_case field.
        // TestConfig has `host` (no dashes available), so use a synthetic case
        // through nested database.pool-size — see the next test for the real
        // pool_size case.
        let content = "host = \"x\"\n";
        let result = validate(content, &path(), &test_ctx(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn normalize_kebab_nested_key_is_valid() {
        // `pool-size` in source → `pool_size` after normalize_table → matches
        // the `pool_size` field on TestDbConfig.
        let content = "[database]\npool-size = 25\n";
        let result = validate(content, &path(), &test_ctx(), true);
        assert!(result.is_ok(), "kebab key should be accepted: {result:?}");
    }

    #[test]
    fn normalize_kebab_typo_reports_line_at_kebab_source() {
        // User typed a kebab-cased typo. After normalize, the reported key is
        // in snake form. The line-number lookup must still locate the kebab
        // line in the original source.
        let content = "host = \"x\"\n[database]\npool-zize = 99\n";
        let result = validate(content, &path(), &test_ctx(), true);
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        // The reported key is in normalized (snake) form …
        assert_eq!(keys[0].key, "database.pool_zize");
        // … but the line still points at the kebab line in the original file.
        assert_eq!(keys[0].line, 3);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "pool-zize");
    }

    #[test]
    fn normalize_kebab_section_header_resolves_line() {
        // Section header itself is kebab in the source. The span index
        // path is rewritten with the table; the key span still points at
        // `my-section` in the header.
        let content = "[my-section]\nfoo = 1\n";
        let err = validate(content, &path(), &test_ctx(), true).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        // Top-level `my_section` is the unknown key here.
        let hit = keys.iter().find(|k| k.key == "my_section").unwrap();
        assert_eq!(hit.line, 1);
    }

    #[test]
    fn normalize_off_treats_kebab_as_unknown() {
        // Sanity check: with normalization disabled, `pool-size` still fails
        // strict validation the way it always has.
        let content = "[database]\npool-size = 25\n";
        let result = validate(content, &path(), &test_ctx(), false);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_key_in_array_of_tables_has_line_and_key_span() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("App")
            .array_of(
                "servers",
                Schema::object("Server").field("host", Field::string().optional()),
            )
            .build();
        let content = "[[servers]]\nhost = \"a\"\n[[servers]]\nrogue = 1\n";
        let parsed = crate::format::TomlAdapter.parse(content).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => unreachable!(),
        };
        let err = validate_unknown(
            &table,
            &schema,
            &UnknownKeySource::File {
                path: &path(),
                source: content,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "servers[1].rogue");
        assert_eq!(keys[0].line, 4);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "rogue");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("rogue = 1"), "{out}");
        assert!(out.contains("^"), "{out}");
        assert!(out.contains(":4"), "{out}");
    }

    #[test]
    fn unknown_key_in_inline_table_has_line_and_key_span() {
        let content = "host = \"x\"\ndatabase = { url = \"pg://\", typo = 1 }\n";
        let err = validate(content, &path(), &test_ctx(), false).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "database.typo");
        assert_eq!(keys[0].line, 2);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "typo");
    }

    #[test]
    fn quoted_dotted_key_is_distinct_from_nested() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("App")
            .nested(
                "a",
                Schema::object("A").field("c", Field::integer().optional()),
            )
            .build();
        let quoted = "\"a.b\" = 1\n";
        let parsed = crate::format::TomlAdapter.parse(quoted).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => unreachable!(),
        };
        let err = validate_unknown(
            &table,
            &schema,
            &UnknownKeySource::File {
                path: &path(),
                source: quoted,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "a.b");
        let span = keys[0].span.expect("quoted key span");
        assert_eq!(&quoted[span.start..span.end], "\"a.b\"");

        let nested = "[a]\nb = 1\n";
        let parsed = crate::format::TomlAdapter.parse(nested).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => unreachable!(),
        };
        let err = validate_unknown(
            &table,
            &schema,
            &UnknownKeySource::File {
                path: &path(),
                source: nested,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "a.b");
        let span = keys[0].span.expect("nested key span");
        assert_eq!(&nested[span.start..span.end], "b");
    }

    #[test]
    fn yaml_and_json_unknown_keys_use_the_span_index() {
        use crate::format::FormatAdapter;
        let yaml = "typo: 1\n";
        let parsed = crate::format::YamlAdapter.parse(yaml).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => panic!("expected map"),
        };
        let err = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::File {
                path: Path::new("/p.yaml"),
                source: yaml,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 1);
        let span = keys[0].span.expect("YAML key span");
        assert_eq!(&yaml[span.start..span.end], "typo");

        let json = "{\"typo\": 1}\n";
        let parsed = crate::format::JsonAdapter.parse(json).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => panic!("expected map"),
        };
        let err = validate_unknown(
            &table,
            &crate::fixtures::test::test_schema(),
            &UnknownKeySource::File {
                path: Path::new("/p.json"),
                source: json,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 1);
        let span = keys[0].span.expect("JSON key span");
        assert_eq!(&json[span.start..span.end], "\"typo\"");
    }

    #[test]
    fn normalize_keys_snippet_carets_the_original_quoted_spelling() {
        let content = "\"my-key\" = 1\n";
        let err = validate(content, &path(), &test_ctx(), true).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "my_key");
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "\"my-key\"");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("\"my-key\" = 1"), "{out}");
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        assert!(
            caret.contains("^^^^^^^^"),
            "caret should cover the quoted original spelling, got: {out}"
        );
    }

    #[test]
    fn unicode_quoted_key_caret_is_character_width() {
        let content = "\"🔑\" = 1\n";
        let err = validate(content, &path(), &test_ctx(), false).unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "\"🔑\"");
        let out = crate::render::render_plain(&err);
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        let caret_run = caret.chars().filter(|&c| c == '^').count();
        assert_eq!(caret_run, 3, "caret should be character-width, got: {out}");
    }

    fn map_of_plugin_schema() -> crate::runtime::Schema {
        use crate::runtime::{Field, Schema};
        Schema::object("App")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("host", Field::string().optional()),
            )
            .build()
    }

    fn unknown_in(
        content: &str,
        adapter: &dyn FormatAdapter,
        file: &str,
    ) -> crate::error::ClapfigError {
        let parsed = adapter.parse(content).unwrap();
        let table = match parsed.value {
            crate::value::Value::Map(map) => map,
            _ => unreachable!(),
        };
        validate_unknown(
            &table,
            &map_of_plugin_schema(),
            &UnknownKeySource::File {
                path: Path::new(file),
                source: content,
                spans: &parsed.spans,
            },
            &test_ctx(),
        )
        .unwrap_err()
    }

    #[test]
    fn unknown_under_quoted_dotted_map_of_key_keeps_the_span() {
        let content = "[plugins.\"acme.prod\"]\nrogue = 1\n";
        let err = unknown_in(content, &crate::format::TomlAdapter, "/p.toml");
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 2);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "rogue");
        let out = crate::render::render_plain(&err);
        assert!(out.contains("rogue = 1"), "{out}");
        assert!(out.contains(":2"), "{out}");
    }

    #[test]
    fn unknown_under_inline_quoted_dotted_map_of_key_keeps_the_span() {
        let content = "plugins = { \"acme.prod\" = { rogue = 1 } }\n";
        let err = unknown_in(content, &crate::format::TomlAdapter, "/p.toml");
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 1);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "rogue");
    }

    #[test]
    fn unknown_under_bracket_bearing_map_of_key_keeps_the_span() {
        // `parse_segment` used to treat `x[0]` as name `x` + index 0.
        let content = "[plugins.\"x[0]\"]\nrogue = 1\n";
        let err = unknown_in(content, &crate::format::TomlAdapter, "/p.toml");
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 2);
        let span = keys[0].span.expect("key span");
        assert_eq!(&content[span.start..span.end], "rogue");
    }

    #[test]
    fn yaml_and_json_quoted_dotted_map_of_key_keeps_the_span() {
        let yaml = "plugins:\n  \"acme.prod\":\n    rogue: 1\n";
        let err = unknown_in(yaml, &crate::format::YamlAdapter, "/p.yaml");
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 3);
        let span = keys[0].span.expect("YAML key span");
        assert_eq!(&yaml[span.start..span.end], "rogue");

        let json = "{\"plugins\":{\"acme.prod\":{\"rogue\":1}}}\n";
        let err = unknown_in(json, &crate::format::JsonAdapter, "/p.json");
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        let span = keys[0].span.expect("JSON key span");
        assert_eq!(&json[span.start..span.end], "\"rogue\"");
    }
}
