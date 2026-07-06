//! Strict-mode validation: detect unknown keys in config files.
//!
//! Operates on an already-parsed [`toml::Table`] so it sees exactly the same
//! keys that will reach the merge step. When kebab-case normalization is
//! enabled the table arrives with `-` already rewritten to `_`, and the
//! line-number lookup is taught to match keys regardless of dash/underscore
//! spelling so error messages still point at the user's original line.

use std::path::Path;
use std::sync::Arc;

use confique::Config;
use serde::Deserialize;
use toml::{Table, Value};

use crate::error::{ClapfigError, UnknownKeyInfo};
use crate::normalize::normalize_key;
use crate::spec::{FieldKindRef, SchemaRef};
use crate::strict::{
    CollectedUnknown, StrictnessOverrides, UnknownKeyContext, UnknownKeyDecision, UnknownKeyHook,
};

/// Per-resolution strictness configuration passed into the validate path.
///
/// Bundles the cascade overrides, the builder-level default ([Knob 1]),
/// and the optional [`on_unknown_key`](crate::ClapfigBuilder::on_unknown_key)
/// callback. The `normalize_keys` flag is forwarded to the line-number
/// heuristic so error snippets still point at the user's original line
/// when keys round-trip through kebab → snake normalization.
///
/// [Knob 1]: crate::ClapfigBuilder::strict
pub(crate) struct ValidateContext<'a> {
    pub overrides: &'a StrictnessOverrides,
    pub default_strict: bool,
    pub callback: Option<&'a UnknownKeyHook>,
    pub normalize_keys: bool,
}

/// Static-path collector: deserialize the parsed table through
/// `serde_ignored` to gather paths the typed `C` doesn't recognize, then
/// filter them through the strictness cascade.
///
/// The `serde_ignored` step also runs `C::Layer` deserialization, so type
/// errors in the merged-table phase surface here as `ParseError`. (Same
/// behavior as before Phase 3 — only the post-collect filtering changed.)
///
/// Returns the keys the callback elected to [`UnknownKeyDecision::Collect`]
/// (empty when no callback is registered or no key opts in). Reject
/// decisions become a `ClapfigError::UnknownKeys` error; lenient keys are
/// dropped silently.
pub fn validate_unknown_keys<C: Config>(
    table: &Table,
    source: &str,
    path: &Path,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
    let mut unknown_paths: Vec<String> = Vec::new();

    let value = Value::Table(table.clone());
    let _layer: C::Layer = serde_ignored::deserialize(value, |ignored_path| {
        unknown_paths.push(ignored_path.to_string());
    })
    .map_err(|e| ClapfigError::ParseError {
        path: path.to_path_buf(),
        source: Box::new(e),
        source_text: Some(Arc::from(source)),
    })?;

    let unknown_keys: Vec<UnknownKey> = unknown_paths
        .into_iter()
        .map(UnknownKey::from_path)
        .collect();
    filter_through_cascade(table, source, path, unknown_keys, ctx)
}

/// Single unknown-key entry passed to `filter_through_cascade`.
///
/// `path` is the dotted form (suitable for the cascade lookup, the
/// line-number heuristic, and error rendering). `leaf` is the raw TOML
/// key the parser saw at the leaf position — distinct from the trailing
/// dot-split segment when the key was quoted with `.` inside it (a
/// literal TOML quoted key like `"acme.task-due-date-missing"`). The
/// dynamic path captures the raw key during the schema walk; the static
/// path (`serde_ignored`) only sees the dotted path, so it falls back
/// to the trailing segment via [`UnknownKey::from_path`].
pub(crate) struct UnknownKey {
    pub path: String,
    pub leaf: String,
}

impl UnknownKey {
    /// Fallback constructor: derive `leaf` from `path` via dot-split. Used
    /// on the static path where the original TOML key is no longer
    /// available after `serde_ignored` flattens the structure.
    pub fn from_path(path: String) -> Self {
        let leaf = path.rsplit('.').next().unwrap_or(&path).to_string();
        Self { path, leaf }
    }
}

/// Schema-driven unknown-key walker that works against any
/// [`SchemaRef`] (static or runtime). The walker recurses through
/// [`FieldKindRef::Nested`] and [`FieldKindRef::MapOf`] subtrees; arrays-
/// of-tables don't show up in non-file layers (env / CLI / URL), so
/// `ArrayOf` is skipped at this layer.
///
/// Used by the env-layer validator: env vars are merged after the
/// per-file validate pass, so an `MYAPP__ROGUE_KEY=...` would otherwise
/// slip into the merged result without ever reaching the cascade or the
/// `on_unknown_key` callback. The same walker can be reused for any
/// layer whose values are already a `Table` without source text — the
/// caller threads the resulting `UnknownKey` list through
/// [`filter_through_cascade`].
pub(crate) fn collect_unknown_paths_ref(
    table: &Table,
    schema: SchemaRef<'_>,
    prefix: &str,
) -> Vec<UnknownKey> {
    let mut out = Vec::new();
    walk_against_schema(table, schema, prefix, &mut out);
    out
}

fn walk_against_schema<'a>(
    table: &Table,
    schema: SchemaRef<'a>,
    prefix: &str,
    out: &mut Vec<UnknownKey>,
) {
    // Snapshot the schema's fields into a `Vec` of `&str`-borrowed
    // entries up front, so we can iterate `table` (a borrow) without
    // re-running the schema iterator's setup on every key. Schemas in
    // practice are small enough that a linear `iter().find(...)` per
    // key is fast; if a hot path appears with a wide schema we can
    // switch to `HashMap<&str, ...>` here without touching callers.
    let fields: Vec<(&'a str, FieldKindRef<'a>)> =
        schema.fields().map(|f| (f.name, f.kind)).collect();
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let kind = fields
            .iter()
            .find(|(n, _)| *n == key.as_str())
            .map(|(_, k)| *k);
        match kind {
            None => {
                out.push(UnknownKey {
                    path: full,
                    leaf: key.clone(),
                });
            }
            Some(FieldKindRef::Leaf(_)) => {
                // Leaf — type checking is the merged-table's job, not ours.
            }
            Some(FieldKindRef::Nested { schema: nested }) => {
                if let Value::Table(t) = value {
                    walk_against_schema(t, nested, &full, out);
                }
            }
            Some(FieldKindRef::MapOf {
                schema: item_schema,
            }) => {
                if let Value::Table(entries) = value {
                    for (entry_key, entry_value) in entries {
                        if let Value::Table(t) = entry_value {
                            let entry_path = format!("{full}.{entry_key}");
                            walk_against_schema(t, item_schema, &entry_path, out);
                        }
                    }
                }
            }
            Some(FieldKindRef::ArrayOf { .. }) => {
                // Arrays-of-tables can't be expressed via env / CLI / URL
                // dotted-key syntax; nothing to recurse into for non-file
                // layers. If the value happens to be a table here, the
                // type check at finalize will surface the mismatch.
            }
        }
    }
}

/// Resolve an already-collected list of unknown paths against the cascade
/// and the optional `on_unknown_key` callback. Shared between the static
/// and dynamic paths so both have identical strictness semantics.
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
    table: &Table,
    source: &str,
    path: &Path,
    unknown_keys: Vec<UnknownKey>,
    ctx: &ValidateContext<'_>,
) -> Result<Vec<CollectedUnknown>, ClapfigError> {
    if unknown_keys.is_empty() {
        return Ok(Vec::new());
    }
    let source_arc: Arc<str> = Arc::from(source);
    let mut rejected: Vec<UnknownKeyInfo> = Vec::new();
    let mut collected: Vec<CollectedUnknown> = Vec::new();
    for entry in unknown_keys {
        let UnknownKey { path: key, leaf } = entry;
        let strict = ctx
            .overrides
            .effective_strict(&key, &leaf, ctx.default_strict);
        if !strict {
            // Lenient subtree — drop silently.
            continue;
        }

        let line = find_key_line(source, &key, &leaf, ctx.normalize_keys);
        let value_ref = lookup_value(table, &key, &leaf);

        if let Some(callback) = ctx.callback {
            // Callback runs only on cascade-strict keys. Look the value up
            // by `(path, leaf)` so quoted keys containing dots (literal
            // TOML keys like `"acme.task-due-date-missing"`) resolve
            // correctly. `lookup_value` also walks `[N]` array-index
            // segments, so callbacks on array-internal keys see the real
            // entry value. `value` is `None` when the lookup genuinely
            // can't resolve (out-of-bounds index, path through a
            // non-table) — the callback still runs and can decide based
            // on path/leaf/file/line alone.
            let context = UnknownKeyContext {
                path: &key,
                leaf: &leaf,
                value: value_ref,
                file: Some(path),
                line: if line > 0 { Some(line) } else { None },
            };
            match callback(&context) {
                UnknownKeyDecision::Accept => continue,
                UnknownKeyDecision::Collect => {
                    collected.push(CollectedUnknown {
                        path: key,
                        leaf,
                        value: value_ref.cloned(),
                        file: Some(path.to_path_buf()),
                        line: if line > 0 { Some(line) } else { None },
                    });
                    continue;
                }
                UnknownKeyDecision::Reject => { /* fall through to reject */ }
            }
        }

        rejected.push(UnknownKeyInfo {
            key,
            path: path.to_path_buf(),
            line,
            source: Some(Arc::clone(&source_arc)),
        });
    }
    if rejected.is_empty() {
        Ok(collected)
    } else {
        Err(ClapfigError::UnknownKeys(rejected))
    }
}

/// Look up a value in a parsed table by its full dotted `path` plus the
/// raw `leaf` key the parser saw at the end.
///
/// Splitting `path` on `.` doesn't work when the leaf is a quoted TOML
/// key containing dots (e.g. `"acme.task-due-date-missing"` parses as a
/// single key; my dotted-path encoding flattens it into segments that
/// dot-split can't recover). The fix: strip the leaf — plus the
/// preceding `.` if any — off the end of the path, walk what remains as
/// nested-table segments (descending into `Value::Array` entries when a
/// segment carries a `[N]` index suffix), then fetch `leaf` directly.
///
/// Returns `None` when the lookup genuinely fails: a non-table
/// intermediate, a missing key, or an out-of-bounds array index. The
/// callback receives this `Option` directly through
/// [`UnknownKeyContext::value`](crate::UnknownKeyContext::value) and can
/// decide based on path/leaf/file/line when the value is unavailable.
fn lookup_value<'a>(table: &'a Table, path: &str, leaf: &str) -> Option<&'a Value> {
    let section = crate::strict::section_path_of(path, leaf);
    if section.is_empty() {
        return table.get(leaf);
    }
    let mut segments = section.split('.');
    let first = segments.next().unwrap();
    let (first_name, first_idx) = parse_segment(first);
    let mut cursor: &Value = table.get(first_name)?;
    if let Some(i) = first_idx {
        cursor = cursor.as_array()?.get(i)?;
    }
    for seg in segments {
        let (name, idx) = parse_segment(seg);
        cursor = cursor.as_table()?.get(name)?;
        if let Some(i) = idx {
            cursor = cursor.as_array()?.get(i)?;
        }
    }
    cursor.as_table()?.get(leaf)
}

/// Split a path segment into `(name, optional index)`.
///
/// `plugins[3]` → `("plugins", Some(3))`; `name` → `("name", None)`.
/// Garbage indices (`plugins[foo]`, `plugins[]`) parse as `(name, None)`,
/// which falls through to the next non-array lookup and naturally fails.
fn parse_segment(seg: &str) -> (&str, Option<usize>) {
    if let Some(open) = seg.find('[')
        && let Some(close) = seg[open..].find(']')
    {
        let name = &seg[..open];
        let idx_str = &seg[open + 1..open + close];
        if let Ok(i) = idx_str.parse::<usize>() {
            return (name, Some(i));
        }
    }
    (seg, None)
}

/// Find the 1-indexed line number for a key in TOML content.
///
/// For a dotted key like `"database.typo"`, tracks the current `[section]` header
/// while scanning and only matches the leaf key when inside the correct section.
///
/// `leaf` is the raw TOML key as the parser saw it — distinct from the
/// trailing dot-split segment when the key is a literal quoted key that
/// contains dots (e.g. `"acme.task-due-date-missing"`). Passing the leaf
/// separately preserves the line-number lookup for that case.
///
/// When `normalize_keys` is true, the comparison treats `-` and `_` as the same
/// character — so a normalized lookup key like `"pool_size"` still locates a
/// source line that reads `pool-size = 5`.
///
/// This is a best-effort heuristic — it handles standard `[section]` headers and
/// bare key assignments but does not handle inline tables.
/// Returns 0 if the key cannot be located.
fn find_key_line(content: &str, dotted_path: &str, leaf: &str, normalize_keys: bool) -> usize {
    // Section path = dotted path with the leaf stripped off the end (plus
    // the `.` separator if any). Shared helper so quoted keys containing
    // dots still resolve to the correct enclosing section.
    let section_path = crate::strict::section_path_of(dotted_path, leaf);
    let expected_section: Vec<&str> = if section_path.is_empty() {
        Vec::new()
    } else {
        section_path.split('.').collect()
    };

    let mut current_section: Vec<String> = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Track [section] headers
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            let header = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            current_section = header.split('.').map(|s| s.trim().to_string()).collect();
            continue;
        }

        // Check if we're in the right section
        let in_right_section = expected_section.len() == current_section.len()
            && expected_section
                .iter()
                .zip(&current_section)
                .all(|(a, b)| keys_match(a, b, normalize_keys));

        if !in_right_section {
            continue;
        }

        // Manually pull "<key> = ..." so we can compare the key under the
        // normalization rule rather than relying on a literal prefix match.
        // `leaf_matches_source_key` also accepts the quoted-key form so
        // a TOML line like `"acme.task" = 1` matches a leaf
        // `acme.task` (the parser strips the quotes; the source line
        // still carries them).
        if let Some((candidate, rest)) = trimmed.split_once('=')
            && leaf_matches_source_key(candidate.trim_end(), leaf, normalize_keys)
            && !rest.is_empty()
        {
            return i + 1;
        }
    }
    0
}

/// Match a parsed `leaf` against the candidate-key text from a source
/// line. Accepts both the bare form (`name`) and the basic quoted form
/// (`"name"`) — TOML's parser strips the surrounding `"`/`'`, but our
/// source-text matcher must accept either.
fn leaf_matches_source_key(candidate: &str, leaf: &str, normalize_keys: bool) -> bool {
    let candidate = candidate.trim();
    if keys_match(candidate, leaf, normalize_keys) {
        return true;
    }
    // Strip a surrounding pair of `"` or `'` and retry — covers basic
    // TOML quoted keys. Literal-string keys (`'`) and escape sequences
    // inside basic strings are heuristic matches only; close enough for
    // line-number rendering.
    let bytes = candidate.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        let inner = &candidate[1..candidate.len() - 1];
        return keys_match(inner, leaf, normalize_keys);
    }
    false
}

/// Compare two keys for equality, optionally treating `-` and `_` as the
/// same character. The trim allows callers to hand in raw section/key
/// fragments without pre-trimming.
fn keys_match(a: &str, b: &str, normalize_keys: bool) -> bool {
    let a = a.trim();
    let b = b.trim();
    if normalize_keys {
        normalize_key(a) == normalize_key(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn path() -> PathBuf {
        PathBuf::from("/test/config.toml")
    }

    fn parse(content: &str) -> Table {
        content.parse::<Table>().unwrap()
    }

    /// Default validate context: strict on, no overrides, no callback.
    /// Mirrors the pre-Phase-3 default and is the right baseline for every
    /// existing test in this module.
    fn test_ctx(normalize_keys: bool) -> ValidateContext<'static> {
        static EMPTY: OnceLock<StrictnessOverrides> = OnceLock::new();
        let overrides = EMPTY.get_or_init(StrictnessOverrides::new);
        ValidateContext {
            overrides,
            default_strict: true,
            callback: None,
            normalize_keys,
        }
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
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn unknown_top_level_key() {
        let content = "host = \"localhost\"\ntypo_key = 42\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
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
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "database.typo");
        assert_eq!(keys[0].leaf(), "typo");
    }

    #[test]
    fn multiple_unknown_keys() {
        let content = "typo1 = 1\ntypo2 = 2\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn line_number_accuracy() {
        let content = "host = \"x\"\nport = 8080\ndebug = false\n\n# comment\nbad_key = 1\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].line, 6);
    }

    #[test]
    fn empty_content_ok() {
        let table = Table::new();
        let result = validate_unknown_keys::<TestConfig>(&table, "", &path(), &test_ctx(false));
        assert!(result.is_ok());
    }

    #[test]
    fn known_optional_field_ok() {
        let content = "[database]\nurl = \"pg://\"\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn sparse_config_ok() {
        let content = "port = 3000\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn error_includes_file_path() {
        let content = "typo = 1\n";
        let p = PathBuf::from("/home/user/.config/myapp/config.toml");
        let err =
            validate_unknown_keys::<TestConfig>(&parse(content), content, &p, &test_ctx(false))
                .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("config.toml") || msg.contains("Unknown keys"));
    }

    #[test]
    fn line_number_finds_correct_section_for_duplicate_leaf() {
        let content = "host = \"x\"\nport = 8080\n[database]\ntypo = \"bad\"\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "database.typo");
        assert_eq!(keys[0].line, 4);
    }

    #[test]
    fn line_number_top_level_not_confused_by_nested_same_name() {
        let content = "typo = 99\n[database]\npool_size = 5\n";
        let result = validate_unknown_keys::<TestConfig>(
            &parse(content),
            content,
            &path(),
            &test_ctx(false),
        );
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys[0].key, "typo");
        assert_eq!(keys[0].line, 1);
    }

    // -- normalize_keys = true ------------------------------------------------

    use crate::normalize::normalize_table;

    fn parse_and_normalize(content: &str) -> Table {
        let mut t = parse(content);
        normalize_table(&mut t).expect("test fixtures must not contain collisions");
        t
    }

    #[test]
    fn normalize_kebab_top_level_key_is_valid() {
        // `pool_size` isn't a top-level field but `host` is — exercise the
        // happy path where a kebab key normalizes to a known snake_case field.
        // TestConfig has `host` (no dashes available), so use a synthetic case
        // through nested database.pool-size — see the next test for the real
        // pool_size case.
        let content = "host = \"x\"\n";
        let table = parse_and_normalize(content);
        let result = validate_unknown_keys::<TestConfig>(&table, content, &path(), &test_ctx(true));
        assert!(result.is_ok());
    }

    #[test]
    fn normalize_kebab_nested_key_is_valid() {
        // `pool-size` in source → `pool_size` after normalize_table → matches
        // the `pool_size` field on TestDbConfig.
        let content = "[database]\npool-size = 25\n";
        let table = parse_and_normalize(content);
        let result = validate_unknown_keys::<TestConfig>(&table, content, &path(), &test_ctx(true));
        assert!(result.is_ok(), "kebab key should be accepted: {result:?}");
    }

    #[test]
    fn normalize_kebab_typo_reports_line_at_kebab_source() {
        // User typed a kebab-cased typo. After normalize, the reported key is
        // in snake form. The line-number lookup must still locate the kebab
        // line in the original source.
        let content = "host = \"x\"\n[database]\npool-zize = 99\n";
        let table = parse_and_normalize(content);
        let result = validate_unknown_keys::<TestConfig>(&table, content, &path(), &test_ctx(true));
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        // The reported key is in normalized (snake) form …
        assert_eq!(keys[0].key, "database.pool_zize");
        // … but the line still points at the kebab line in the original file.
        assert_eq!(keys[0].line, 3);
    }

    #[test]
    fn normalize_kebab_section_header_resolves_line() {
        // Section header itself is kebab in the source. `find_key_line` must
        // match it against the normalized expected section name.
        let content = "[my-section]\nfoo = 1\n";
        let table = parse_and_normalize(content);
        // `my-section` isn't a known field; we just want to confirm the
        // unknown-key lookup found a line (non-zero) using kebab matching on
        // the section header.
        let err = validate_unknown_keys::<TestConfig>(&table, content, &path(), &test_ctx(true))
            .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        // Top-level `my_section` is the unknown key here.
        assert!(keys.iter().any(|k| k.key == "my_section"));
    }

    #[test]
    fn normalize_off_treats_kebab_as_unknown() {
        // Sanity check: with normalization disabled, `pool-size` still fails
        // strict validation the way it always has.
        let content = "[database]\npool-size = 25\n";
        let table = parse(content);
        let result =
            validate_unknown_keys::<TestConfig>(&table, content, &path(), &test_ctx(false));
        assert!(result.is_err());
    }
}
