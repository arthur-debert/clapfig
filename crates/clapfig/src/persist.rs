//! Config persistence: patch values into config files while preserving
//! formatting.
//!
//! All source-text mechanics route through the format adapter contract
//! ([`FormatAdapter::edit`]) — for TOML that is lossless
//! comment-preserving editing. This module owns the format-agnostic half:
//! key/type validation against the schema, template seeding for missing
//! files, classifying each set request onto its capability-matrix row
//! ([`SetTarget`] — replace vs create-key vs create-file, so refusals
//! name the operation actually attempted), key-spelling resolution under
//! [`normalize_keys`](crate::RuntimeBuilder::normalize_keys) (dash and
//! underscore spellings are equivalent; the spelling already present in
//! the document is the one edited, and a document holding both
//! equivalent spellings anywhere errors as a collision rather than one
//! spelling silently winning), and the file I/O around each edit.

use std::path::Path;

use crate::error::ClapfigError;
use crate::format::{ConfigPath, FileEdit, FormatAdapter, Operation, SetTarget};
use crate::normalize::{
    KeyCollision, check_collisions, kebab_key, normalize_key, resolve_table_key,
};
use crate::ops::ConfigResult;
use crate::value::Value;

/// Pure function: patch a config document string, setting `key` to
/// `raw_value`, through `adapter`.
///
/// Validates the key against an owned [`Schema`](crate::runtime::Schema)
/// and the value against the leaf's declared
/// [`LeafType`](crate::runtime::LeafType) — with schema-driven datetime
/// coercion for `DateTime` leaves (ADR-0001) — so a typo in the key name
/// or a string where an integer is expected fails before the file is
/// touched.
///
/// With `normalize_keys`, the action key follows the load path's
/// acceptance: dash and underscore spellings are equivalent. The key is
/// normalized to the canonical snake_case path before schema validation,
/// paths already present in the document are resolved by that equivalence
/// and edited under the spelling actually present (so setting `pool_size`
/// against a kebab-case document edits `pool-size` instead of creating a
/// colliding sibling), and paths not present are emitted kebab-case —
/// matching what `config gen` emits. A document already holding BOTH
/// equivalent spellings ANYWHERE — even at a key or table the edit never
/// touches — is ambiguous and fails to load, so the edit fails with
/// [`ClapfigError::NormalizedKeyCollision`] instead of editing a
/// document the load path refuses (the I/O wrapper stamps the file path;
/// from this pure function the error's path is empty).
///
/// If `content` is `None` (file doesn't exist yet), starts from the
/// adapter's generated template — rendered with the same `normalize_keys`
/// setting, so the seeded file and `config gen` agree on key spelling —
/// so the new file carries doc comments.
///
/// The edit request carries the capability-matrix row actually attempted
/// ([`SetTarget`]): a missing file is [`Operation::EditCreateFile`]
/// (required up-front, before template seeding), and an existing document
/// is parsed to classify replace ([`Operation::EditSet`]) vs create-key
/// ([`Operation::EditCreateKey`]) — so typed refusals name the operation
/// the caller attempted, not a blanket "set". Parsing is the edit's own
/// first step, so a document the adapter cannot parse fails as its parse
/// error.
///
/// Schema/key validation failures are [`ClapfigError::KeyNotFound`] /
/// [`ClapfigError::InvalidValue`]; adapter edit failures — including the
/// typed [`UnsupportedByFormat`](crate::format::UnsupportedByFormat)
/// refusal and path conflicts — propagate as [`ClapfigError::Format`],
/// preserving the full [`FormatError`](crate::format::FormatError).
///
/// Returns the modified document string.
pub fn set_in_document_runtime(
    adapter: &dyn FormatAdapter,
    schema: &crate::runtime::Schema,
    content: Option<&str>,
    key: &str,
    raw_value: &str,
    normalize_keys: bool,
) -> Result<String, ClapfigError> {
    let canonical = canonical_key(key, normalize_keys);
    let valid_keys = crate::overrides::valid_keys(crate::spec::SchemaRef::from_dynamic(schema));
    if !valid_keys.contains(&canonical) {
        return Err(ClapfigError::KeyNotFound(key.into()));
    }

    let mut value = parse_raw_value(raw_value);
    if let Some(leaf_ty) = lookup_leaf_type(schema, &canonical) {
        crate::runtime_spec::coerce_datetime_value(&mut value, leaf_ty);
        leaf_ty
            .check(&value)
            .map_err(|reason| ClapfigError::InvalidValue {
                key: key.into(),
                reason,
            })?;
    }

    let (base, target, path) = match content {
        Some(c) => {
            // Replace vs create-key depends on whether the path already
            // resolves; classification parses the document — the edit's
            // own first step, so parse failures (including a format that
            // cannot parse at all) surface as-is. The same parsed tree
            // resolves the concrete key spellings the edit targets.
            let tree = adapter.parse(c).map_err(ClapfigError::from)?;
            let (segments, exists) = resolve_document_path(&tree, &canonical, normalize_keys)
                .map_err(|c| c.into_error(Path::new("")))?;
            let target = if exists {
                SetTarget::ExistingValue
            } else {
                SetTarget::MissingKey
            };
            (c.to_string(), target, config_path(&segments))
        }
        None => {
            // Missing file: require the matrix row before template
            // seeding, so the refusal names the attempted operation
            // rather than template generation.
            adapter
                .require(Operation::EditCreateFile)
                .map_err(crate::format::FormatError::from)
                .map_err(ClapfigError::from)?;
            let seeded = crate::ops::generate_template(adapter, schema, normalize_keys)?;
            // The seeded template spells every key the way the enabled
            // normalization emits (kebab-case when on), so the edit path
            // uses that emitted spelling and lands on the template's own
            // keys instead of creating colliding siblings.
            let segments: Vec<String> = canonical
                .split('.')
                .map(|seg| emitted_spelling(seg, normalize_keys))
                .collect();
            (seeded, SetTarget::MissingFile, config_path(&segments))
        }
    };
    let base = if base.trim().is_empty() {
        String::new()
    } else {
        base
    };

    adapter
        .edit(
            &base,
            FileEdit::Set {
                path: &path,
                value: &value,
                target,
            },
        )
        .map_err(ClapfigError::from)
}

/// Stamp `file_path` onto a collision error raised by a document-level
/// (pure, pathless) persist function, so the reported error names the
/// file actually edited — the same shape the load path reports.
fn stamp_collision_path(err: ClapfigError, file_path: &Path) -> ClapfigError {
    match err {
        ClapfigError::NormalizedKeyCollision {
            path,
            section,
            normalized_key,
            originals,
        } if path.as_os_str().is_empty() => ClapfigError::NormalizedKeyCollision {
            path: file_path.to_path_buf(),
            section,
            normalized_key,
            originals,
        },
        other => other,
    }
}

/// Wrapper around [`set_in_document_runtime`] with file I/O: reads the file
/// (if it exists), patches it, writes back. Creates parent directories if
/// needed. Collision errors from the document layer get this file's path.
pub fn persist_value_runtime(
    adapter: &dyn FormatAdapter,
    schema: &crate::runtime::Schema,
    file_path: &Path,
    key: &str,
    value: &str,
    normalize_keys: bool,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let new_content = set_in_document_runtime(
        adapter,
        schema,
        content.as_deref(),
        key,
        value,
        normalize_keys,
    )
    .map_err(|e| stamp_collision_path(e, file_path))?;

    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ClapfigError::IoError {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    Ok(ConfigResult::ValueSet {
        key: key.into(),
        value: value.into(),
    })
}

/// Descend a runtime schema by dotted key path and return the target leaf's
/// declared type. `None` when the path doesn't resolve to a leaf.
fn lookup_leaf_type<'a>(
    schema: &'a crate::runtime::Schema,
    dotted: &str,
) -> Option<&'a crate::runtime::LeafType> {
    let mut current = schema;
    let mut segments = dotted.split('.').peekable();
    while let Some(seg) = segments.next() {
        let nf = current.fields.iter().find(|f| f.name == seg)?;
        match &nf.field {
            crate::runtime::Field::Leaf(leaf) if segments.peek().is_none() => {
                return Some(&leaf.ty);
            }
            crate::runtime::Field::Nested(inner) if segments.peek().is_some() => {
                current = inner;
            }
            _ => return None,
        }
    }
    None
}

/// Pure function: remove a key from a config document string through
/// `adapter`.
///
/// If the key doesn't exist, returns the document unchanged.
/// Navigates dotted key paths (e.g. `"database.pool_size"`). With
/// `normalize_keys`, the document is parsed first and the key is resolved
/// by dash/underscore equivalence, so `unset pool_size` removes an
/// existing `pool-size` entry (and vice versa) — parse failures propagate
/// in that mode, and a document holding both equivalent spellings
/// anywhere fails with [`ClapfigError::NormalizedKeyCollision`] (empty
/// path here; the I/O wrapper stamps the file) rather than editing a
/// document the load path refuses. Comment preservation is per the
/// adapter's declared edit capability; adapter failures propagate as
/// [`ClapfigError::Format`].
///
/// Returns the modified document string.
pub fn unset_in_document(
    adapter: &dyn FormatAdapter,
    content: &str,
    key: &str,
    normalize_keys: bool,
) -> Result<String, ClapfigError> {
    let path = if normalize_keys {
        let tree = adapter.parse(content).map_err(ClapfigError::from)?;
        let (segments, _) = resolve_document_path(&tree, &normalize_key(key), true)
            .map_err(|c| c.into_error(Path::new("")))?;
        config_path(&segments)
    } else {
        dotted_config_path(key)
    };
    adapter
        .edit(content, FileEdit::Unset { path: &path })
        .map_err(ClapfigError::from)
}

/// I/O wrapper: reads file, removes the key, writes back.
/// If the file doesn't exist, succeeds silently (nothing to unset).
pub fn unset_value(
    adapter: &dyn FormatAdapter,
    file_path: &Path,
    key: &str,
    normalize_keys: bool,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigResult::ValueUnset { key: key.into() });
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let new_content = unset_in_document(adapter, &content, key, normalize_keys)
        .map_err(|e| stamp_collision_path(e, file_path))?;

    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    Ok(ConfigResult::ValueUnset { key: key.into() })
}

/// The canonical snake_case form of a user-supplied action key: with
/// `normalize_keys`, dashes rewrite to underscores (the schema's spelling);
/// without it, the key passes through untouched (exact spelling required,
/// matching the load path's acceptance).
fn canonical_key(key: &str, normalize_keys: bool) -> String {
    if normalize_keys {
        normalize_key(key)
    } else {
        key.to_owned()
    }
}

/// The spelling emitted into a document for a path segment that is not
/// already present: kebab-case when normalization is enabled (matching
/// `config gen` output), the canonical snake_case otherwise.
fn emitted_spelling(segment: &str, normalize_keys: bool) -> String {
    if normalize_keys {
        kebab_key(segment)
    } else {
        segment.to_owned()
    }
}

/// Resolve a canonical snake_case dotted path against a parsed document
/// tree: the concrete per-segment spellings an edit must target, plus
/// whether the full path already resolves to a value — the
/// [`SetTarget::ExistingValue`] vs [`SetTarget::MissingKey`]
/// classification for a set against an existing file.
///
/// With `normalize_keys`, the WHOLE document tree is first validated
/// with [`check_collisions`]: any table holding more than one equivalent
/// spelling (`pool-size` AND `pool_size`) — even one the requested path
/// never traverses — is ambiguous and errors with [`KeyCollision`]. The
/// same never-silently-compete rule the load path enforces, so an edit
/// can never touch a document the load path refuses. Each segment then
/// matches an existing key by dash/underscore equivalence
/// ([`resolve_table_key`]); segments with no match resolve to their
/// emitted (kebab-case) spelling. Without normalization, matching is
/// exact and missing segments keep their canonical spelling.
fn resolve_document_path(
    tree: &Value,
    canonical: &str,
    normalize_keys: bool,
) -> Result<(Vec<String>, bool), KeyCollision> {
    if normalize_keys && let Value::Map(map) = tree {
        check_collisions(map)?;
    }
    let mut segments: Vec<String> = Vec::new();
    let mut current = Some(tree);
    let mut exists = true;
    for seg in canonical.split('.') {
        let matched = match current {
            Some(Value::Map(map)) => {
                if normalize_keys {
                    resolve_table_key(map, seg).cloned()
                } else if map.contains_key(seg) {
                    Some(seg.to_owned())
                } else {
                    None
                }
            }
            _ => None,
        };
        match matched {
            Some(spelling) => {
                current = match current {
                    Some(Value::Map(map)) => map.get(&spelling),
                    _ => None,
                };
                segments.push(spelling);
            }
            None => {
                exists = false;
                current = None;
                segments.push(emitted_spelling(seg, normalize_keys));
            }
        }
    }
    Ok((segments, exists))
}

/// Build a structured [`ConfigPath`] from already-resolved path segments.
fn config_path(segments: &[String]) -> ConfigPath {
    let mut path = ConfigPath::new();
    for segment in segments {
        path = path.key(segment);
    }
    path
}

/// Build a structured [`ConfigPath`] from a dotted persist key. Persist
/// keys are schema-validated dotted paths, so every `.` is a nesting
/// separator (schema field names cannot contain dots).
fn dotted_config_path(key: &str) -> ConfigPath {
    let mut path = ConfigPath::new();
    for segment in key.split('.') {
        path = path.key(segment);
    }
    path
}

/// Parse a raw `config set` string into a typed config value with the
/// same bool > integer > float > string heuristic as env/URL values.
///
/// The parsed value is checked against the target leaf's declared
/// [`LeafType`](crate::runtime::LeafType) to catch type mismatches before
/// persisting.
fn parse_raw_value(s: &str) -> Value {
    crate::env::parse_env_value(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::{enum_schema, test_schema};
    use crate::format::TomlAdapter;
    use std::fs;
    use tempfile::TempDir;

    fn set_in_document(
        content: Option<&str>,
        key: &str,
        value: &str,
    ) -> Result<String, ClapfigError> {
        set_in_document_runtime(&TomlAdapter, &test_schema(), content, key, value, false)
    }

    fn persist_value(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> Result<ConfigResult, ClapfigError> {
        persist_value_runtime(&TomlAdapter, &test_schema(), path, key, value, false)
    }

    // --- validation tests ---

    /// In-test adapter that parses like TOML but declares no edit rows:
    /// set-target classification succeeds, and every edit refuses with
    /// the request's own operation — the shape a partially-implemented
    /// adapter (parse landed, edits not yet) presents.
    struct ParseOnly;

    impl FormatAdapter for ParseOnly {
        fn name(&self) -> &'static str {
            "parseonly"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["po"]
        }

        fn capabilities(&self) -> &'static [crate::format::Operation] {
            &[crate::format::Operation::Parse]
        }

        fn parse(&self, text: &str) -> Result<Value, crate::format::FormatError> {
            TomlAdapter.parse(text)
        }

        fn serialize(&self, _value: &Value) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Serialize)
                .unwrap_err()
                .into())
        }

        fn template(
            &self,
            _schema: &crate::runtime::Schema,
        ) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Template)
                .unwrap_err()
                .into())
        }

        fn edit(
            &self,
            _source: &str,
            edit: FileEdit<'_>,
        ) -> Result<String, crate::format::FormatError> {
            Err(self.require(edit.operation()).unwrap_err().into())
        }

        fn span_index(
            &self,
            _text: &str,
        ) -> Result<
            std::collections::BTreeMap<crate::format::ConfigPath, crate::format::Span>,
            crate::format::FormatError,
        > {
            Err(self
                .require(crate::format::Operation::SpanIndex)
                .unwrap_err()
                .into())
        }
    }

    /// Unwrap a persist error down to the typed refusal.
    fn refusal(result: Result<String, ClapfigError>) -> crate::format::UnsupportedByFormat {
        match result.unwrap_err() {
            ClapfigError::Format(crate::format::FormatError::Unsupported(u)) => u,
            other => panic!("expected typed refusal, got {other:?}"),
        }
    }

    #[test]
    fn set_refusals_name_the_attempted_matrix_row() {
        // The refusal reports the capability-matrix row the caller
        // actually attempted — replace, create-key, or create-file — not
        // a blanket "replacing an existing value" for every set.
        let schema = test_schema();

        // Key exists in the document → replacing an existing value.
        let u = refusal(set_in_document_runtime(
            &ParseOnly,
            &schema,
            Some("port = 1\n"),
            "port",
            "2",
            false,
        ));
        assert_eq!(u.operation, Operation::EditSet);

        // File exists, key does not → creating a missing key.
        let u = refusal(set_in_document_runtime(
            &ParseOnly,
            &schema,
            Some("port = 1\n"),
            "debug",
            "true",
            false,
        ));
        assert_eq!(u.operation, Operation::EditCreateKey);

        // No file at all → creating a missing file, refused before any
        // template seeding (ParseOnly's template also refuses; the
        // create-file refusal must win).
        let u = refusal(set_in_document_runtime(
            &ParseOnly, &schema, None, "port", "1", false,
        ));
        assert_eq!(u.operation, Operation::EditCreateFile);
    }

    /// In-test adapter that declares nothing at all — the shape a
    /// not-yet-implemented format presents (every built-in adapter is
    /// implemented now, so the shape only exists in-test).
    struct RefusesAll;

    impl FormatAdapter for RefusesAll {
        fn name(&self) -> &'static str {
            "refusesall"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["ra"]
        }

        fn capabilities(&self) -> &'static [crate::format::Operation] {
            &[]
        }

        fn parse(&self, _text: &str) -> Result<Value, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Parse)
                .unwrap_err()
                .into())
        }

        fn serialize(&self, _value: &Value) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Serialize)
                .unwrap_err()
                .into())
        }

        fn template(
            &self,
            _schema: &crate::runtime::Schema,
        ) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Template)
                .unwrap_err()
                .into())
        }

        fn edit(
            &self,
            _source: &str,
            edit: FileEdit<'_>,
        ) -> Result<String, crate::format::FormatError> {
            Err(self.require(edit.operation()).unwrap_err().into())
        }

        fn span_index(
            &self,
            _text: &str,
        ) -> Result<
            std::collections::BTreeMap<crate::format::ConfigPath, crate::format::Span>,
            crate::format::FormatError,
        > {
            Err(self
                .require(crate::format::Operation::SpanIndex)
                .unwrap_err()
                .into())
        }
    }

    #[test]
    fn set_on_unparseable_format_fails_at_parse() {
        // A format that cannot parse at all fails classification at its
        // parse refusal — editing an existing file begins with reading
        // it, so that is the honest earliest error.
        let u = refusal(set_in_document_runtime(
            &RefusesAll,
            &test_schema(),
            Some("port = 1\n"),
            "port",
            "2",
            false,
        ));
        assert_eq!(u.operation, Operation::Parse);
    }

    #[test]
    fn set_rejects_unknown_key() {
        let result = set_in_document(Some(""), "nonexistent", "value");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn set_rejects_invalid_enum_value() {
        let result = set_in_document_runtime(
            &TomlAdapter,
            &enum_schema(),
            Some(""),
            "mode",
            "garbage",
            false,
        );
        match result {
            Err(ClapfigError::InvalidValue { key, reason }) => {
                assert_eq!(key, "mode");
                assert!(
                    reason.contains("not in allowed set"),
                    "expected 'not in allowed set' in: {reason}"
                );
            }
            other => panic!("Expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_accepts_valid_enum_value() {
        let result = set_in_document_runtime(
            &TomlAdapter,
            &enum_schema(),
            Some(""),
            "mode",
            "fast",
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn set_rejects_wrong_type() {
        let result = set_in_document(Some(""), "port", "not_a_number");
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
    }

    #[test]
    fn set_rejects_path_through_scalar() {
        // Existing file has `database` as a scalar string; `config set
        // database.url x` would dereference into a non-table item, which
        // pre-fix would panic inside the TOML editor's IndexMut. The
        // guard turns it into the adapter's typed edit failure, which
        // propagates as ClapfigError::Format (never collapsed into
        // InvalidValue — that variant is for schema/type validation).
        let content = "database = \"oops\"\n";
        let result = set_in_document(Some(content), "database.url", "pg://x");
        match result {
            Err(ClapfigError::Format(crate::format::FormatError::Edit {
                format, message, ..
            })) => {
                assert_eq!(format, "toml");
                assert!(message.contains("path conflict"), "got: {message}");
            }
            other => panic!("expected Format(Edit), got {other:?}"),
        }
    }

    #[test]
    fn persist_rejects_invalid_enum_value() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_value_runtime(
            &TomlAdapter,
            &enum_schema(),
            &path,
            "mode",
            "garbage",
            false,
        );
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
        // File should NOT have been created
        assert!(!path.exists());
    }

    #[test]
    fn set_existing_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = set_in_document(Some(content), "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn set_nested_key() {
        let content = "[database]\npool_size = 5\n";
        let result = set_in_document(Some(content), "database.pool_size", "20").unwrap();
        assert!(result.contains("pool_size = 20"));
    }

    #[test]
    fn set_new_key_in_existing_file() {
        let content = "port = 8080\n";
        let result = set_in_document(Some(content), "debug", "true").unwrap();
        assert!(result.contains("debug = true"));
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn set_creates_from_template_when_none() {
        let result = set_in_document(None, "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
    }

    #[test]
    fn preserves_comments() {
        let content = "# This is my config\nport = 8080\n# end\n";
        let result = set_in_document(Some(content), "port", "3000").unwrap();
        assert!(result.contains("# This is my config"));
        assert!(result.contains("port = 3000"));
    }

    #[test]
    fn value_parsing_heuristics() {
        assert!(matches!(parse_raw_value("42"), Value::Integer(42)));
        assert!(matches!(parse_raw_value("true"), Value::Boolean(true)));
        assert!(matches!(parse_raw_value("hello"), Value::String(_)));
        assert!(matches!(parse_raw_value("1.5"), Value::Float(_)));
    }

    #[test]
    fn set_coerces_datetime_string_for_datetime_leaf() {
        // Schema-driven datetime coercion (ADR-0001) applies to `config
        // set` too: the heuristic parses the raw string as a String, and
        // the DateTime leaf declaration coerces it before the type check.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field("stamp", Field::datetime().optional())
            .build();
        let result = set_in_document_runtime(
            &TomlAdapter,
            &schema,
            Some(""),
            "stamp",
            "2024-01-02T03:04:05Z",
            false,
        )
        .unwrap();
        assert!(
            result.contains("stamp = 2024-01-02T03:04:05Z"),
            "datetime must persist unquoted (typed), got: {result}"
        );

        let err = set_in_document_runtime(
            &TomlAdapter,
            &schema,
            Some(""),
            "stamp",
            "not-a-date",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, ClapfigError::InvalidValue { .. }));
    }

    #[test]
    fn persist_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_value(&path, "port", "3000").unwrap();
        assert!(matches!(result, ConfigResult::ValueSet { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
    }

    #[test]
    fn persist_modifies_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\n").unwrap();

        persist_value(&path, "port", "3000").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
        assert!(!content.contains("8080"));
    }

    #[test]
    fn persist_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("dir").join("config.toml");

        persist_value(&path, "port", "3000").unwrap();
        assert!(path.exists());
    }

    // --- normalize_keys tests ---
    //
    // The persistence path must match the load path's acceptance: with
    // normalization on, dash and underscore action-key spellings are
    // equivalent, edits land on the spelling the document already uses,
    // and missing paths/files come out kebab-case. Format-agnostic by
    // construction (the logic lives before the adapter seam), so every
    // scenario runs against all three adapters.

    use crate::format::{JsonAdapter, YamlAdapter};

    /// Per-adapter document with the schema's `database.pool_size` leaf
    /// spelled in the given (kebab or snake) form.
    fn docs(leaf: &str) -> [(&'static dyn FormatAdapter, String); 3] {
        [
            (&TomlAdapter, format!("[database]\n{leaf} = 5\n")),
            (&YamlAdapter, format!("database:\n  {leaf}: 5\n")),
            (
                &JsonAdapter,
                format!("{{\n  \"database\": {{\n    \"{leaf}\": 5\n  }}\n}}\n"),
            ),
        ]
    }

    fn doc_map(adapter: &dyn FormatAdapter, text: &str) -> crate::value::Map {
        match adapter.parse(text).unwrap() {
            Value::Map(m) => m,
            other => panic!("expected map root, got {other:?}"),
        }
    }

    #[test]
    fn normalized_set_edits_kebab_document_for_both_spellings() {
        // The collision scenario from #122: setting the snake key against
        // a kebab document must edit the existing kebab entry, never
        // create a snake sibling (which would NormalizedKeyCollision on
        // the next load). The kebab action key must work too.
        for (adapter, doc) in docs("pool-size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = set_in_document_runtime(
                    adapter,
                    &test_schema(),
                    Some(&doc),
                    action_key,
                    "20",
                    true,
                )
                .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool-size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool_size"),
                    "{} / {action_key} created a colliding snake sibling:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_edits_snake_document_for_both_spellings() {
        // Equivalence is symmetric: a document still spelled snake_case
        // keeps its spelling — the edit targets what is present, it does
        // not rewrite the document to kebab.
        for (adapter, doc) in docs("pool_size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = set_in_document_runtime(
                    adapter,
                    &test_schema(),
                    Some(&doc),
                    action_key,
                    "20",
                    true,
                )
                .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool_size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool-size"),
                    "{} / {action_key} created a colliding kebab sibling:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_emits_kebab_for_missing_paths() {
        // A path not present in the document is created in the emitted
        // (kebab) spelling — the same spelling `config gen` produces.
        let bases: [(&dyn FormatAdapter, &str); 3] = [
            (&TomlAdapter, "host = \"h\"\n"),
            (&YamlAdapter, "host: h\n"),
            (&JsonAdapter, "{\n  \"host\": \"h\"\n}\n"),
        ];
        for (adapter, base) in bases {
            let out = set_in_document_runtime(
                adapter,
                &test_schema(),
                Some(base),
                "database.pool_size",
                "20",
                true,
            )
            .unwrap();
            let map = doc_map(adapter, &out);
            let db = map["database"].as_map().unwrap();
            assert_eq!(
                db.get("pool-size"),
                Some(&Value::Integer(20)),
                "{}:\n{out}",
                adapter.name()
            );
            assert!(!db.contains_key("pool_size"), "{}:\n{out}", adapter.name());
        }
    }

    #[test]
    fn normalized_set_seeds_missing_file_with_normalized_template() {
        // Missing files seed from the template rendered with the SAME
        // normalization setting, so the seeded file and `config gen`
        // agree on key spelling — and the set lands on the template's own
        // kebab key instead of adding a snake duplicate.
        let adapters: [&dyn FormatAdapter; 3] = [&TomlAdapter, &YamlAdapter, &JsonAdapter];
        for adapter in adapters {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out =
                    set_in_document_runtime(adapter, &test_schema(), None, action_key, "20", true)
                        .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool-size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool_size"),
                    "{} / {action_key} seeded a snake key:\n{out}",
                    adapter.name()
                );
                assert!(
                    !out.contains("pool_size"),
                    "{} / {action_key} snake spelling leaked into seeded file:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_validates_kebab_key_against_canonical_schema() {
        // The kebab action key is accepted (normalized before the
        // valid-keys check) and its value is still type-checked against
        // the canonical leaf.
        let result = set_in_document_runtime(
            &TomlAdapter,
            &test_schema(),
            Some(""),
            "database.pool-size",
            "not_a_number",
            true,
        );
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
    }

    #[test]
    fn kebab_action_key_still_rejected_without_normalization() {
        // Acceptance boundary: with normalization off, the load path
        // rejects kebab keys, so the persistence path does too.
        let result = set_in_document(Some(""), "database.pool-size", "20");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn normalized_unset_removes_equivalent_spelling() {
        for (adapter, doc) in docs("pool-size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = unset_in_document(adapter, &doc, action_key, true).unwrap();
                let map = doc_map(adapter, &out);
                let db = map
                    .get("database")
                    .map(|d| d.as_map().unwrap().clone())
                    .unwrap_or_default();
                assert!(
                    !db.contains_key("pool-size") && !db.contains_key("pool_size"),
                    "{} / {action_key} left the key behind:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    /// Per-adapter document with BOTH spellings of the pool-size leaf —
    /// a file the load path refuses as a NormalizedKeyCollision.
    fn colliding_docs() -> [(&'static dyn FormatAdapter, &'static str); 3] {
        [
            (&TomlAdapter, "[database]\npool-size = 5\npool_size = 6\n"),
            (&YamlAdapter, "database:\n  pool-size: 5\n  pool_size: 6\n"),
            (
                &JsonAdapter,
                "{\n  \"database\": {\n    \"pool-size\": 5,\n    \"pool_size\": 6\n  }\n}\n",
            ),
        ]
    }

    /// Unwrap a persist error down to the collision, asserting its shape.
    fn assert_collision(
        result: Result<String, ClapfigError>,
        section: &str,
        normalized_key: &str,
        originals: &[&str],
        context: &str,
    ) {
        match result.unwrap_err() {
            ClapfigError::NormalizedKeyCollision {
                section: s,
                normalized_key: n,
                originals: o,
                ..
            } => {
                assert_eq!(s, section, "{context}");
                assert_eq!(n, normalized_key, "{context}");
                assert_eq!(o, originals, "{context}");
            }
            other => panic!("{context}: expected NormalizedKeyCollision, got {other:?}"),
        }
    }

    #[test]
    fn normalized_set_rejects_ambiguous_leaf_spellings() {
        // A document holding BOTH equivalent spellings fails to load, so
        // set must refuse with the same collision instead of silently
        // editing one of the two entries and leaving the file unloadable.
        for (adapter, doc) in colliding_docs() {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let result = set_in_document_runtime(
                    adapter,
                    &test_schema(),
                    Some(doc),
                    action_key,
                    "20",
                    true,
                );
                assert_collision(
                    result,
                    "database",
                    "pool_size",
                    &["pool-size", "pool_size"],
                    &format!("{} / {action_key}", adapter.name()),
                );
            }
        }
    }

    #[test]
    fn normalized_set_rejects_ambiguous_intermediate_table() {
        // The collision check runs at EVERY traversed table, not just the
        // leaf: two spellings of an intermediate section are just as
        // ambiguous. Section is empty for a root-level collision.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .nested(
                "my_db",
                Schema::object("D").field("size", Field::integer().optional()),
            )
            .build();
        let doc = "[my-db]\nsize = 1\n\n[my_db]\nsize = 2\n";
        let result =
            set_in_document_runtime(&TomlAdapter, &schema, Some(doc), "my_db.size", "3", true);
        assert_collision(result, "", "my_db", &["my-db", "my_db"], "intermediate");
    }

    #[test]
    fn normalized_set_rejects_collision_off_the_requested_path() {
        // The whole document is validated, not just the traversed path:
        // the requested key (`host`, root level) is unambiguous, but the
        // untraversed `database` table holds both spellings — the edit
        // still fails, because a document the load path refuses is never
        // edited.
        for (adapter, doc) in colliding_docs() {
            let doc = with_host(adapter, doc);
            let result =
                set_in_document_runtime(adapter, &test_schema(), Some(&doc), "host", "h2", true);
            assert_collision(
                result,
                "database",
                "pool_size",
                &["pool-size", "pool_size"],
                adapter.name(),
            );
        }
    }

    #[test]
    fn normalized_unset_rejects_collision_off_the_requested_path() {
        // Unset runs the same whole-document validation.
        for (adapter, doc) in colliding_docs() {
            let doc = with_host(adapter, doc);
            let result = unset_in_document(adapter, &doc, "host", true);
            assert_collision(
                result,
                "database",
                "pool_size",
                &["pool-size", "pool_size"],
                adapter.name(),
            );
        }
    }

    /// The colliding fixture with an unambiguous root-level `host` key
    /// added, so a test can target a key that never touches the
    /// colliding table.
    fn with_host(adapter: &dyn FormatAdapter, doc: &str) -> String {
        match adapter.name() {
            "toml" => format!("host = \"h\"\n{doc}"),
            "yaml" => format!("host: h\n{doc}"),
            "json" => doc.replacen("{\n", "{\n  \"host\": \"h\",\n", 1),
            other => panic!("unexpected adapter {other}"),
        }
    }

    #[test]
    fn normalized_unset_rejects_ambiguous_spellings() {
        // Unset resolves through the same collision-aware traversal.
        for (adapter, doc) in colliding_docs() {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let result = unset_in_document(adapter, doc, action_key, true);
                assert_collision(
                    result,
                    "database",
                    "pool_size",
                    &["pool-size", "pool_size"],
                    &format!("{} / {action_key}", adapter.name()),
                );
            }
        }
    }

    #[test]
    fn persist_collision_error_carries_file_path() {
        // The document-level functions have no file path; the I/O
        // wrappers stamp the edited file onto the collision so the error
        // matches the load path's shape.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[database]\npool-size = 5\npool_size = 6\n").unwrap();

        for result in [
            persist_value_runtime(
                &TomlAdapter,
                &test_schema(),
                &path,
                "database.pool_size",
                "20",
                true,
            ),
            unset_value(&TomlAdapter, &path, "database.pool_size", true),
        ] {
            match result.unwrap_err() {
                ClapfigError::NormalizedKeyCollision { path: reported, .. } => {
                    assert_eq!(reported, path);
                }
                other => panic!("expected NormalizedKeyCollision, got {other:?}"),
            }
        }
        // The ambiguous file was not modified.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("pool-size = 5") && content.contains("pool_size = 6"));
    }

    #[test]
    fn normalized_unset_missing_key_is_noop() {
        let out =
            unset_in_document(&TomlAdapter, "host = \"h\"\n", "database.pool_size", true).unwrap();
        assert!(out.contains("host = \"h\""));
    }

    #[test]
    fn unset_without_normalization_requires_exact_spelling() {
        // With normalization off there is no equivalence: a kebab entry
        // is not touched by the snake key (and would be an unknown key at
        // load time anyway).
        let out = unset_doc("[database]\npool-size = 5\n", "database.pool_size").unwrap();
        assert!(out.contains("pool-size = 5"));
    }

    // --- unset tests ---

    fn unset_doc(content: &str, key: &str) -> Result<String, ClapfigError> {
        unset_in_document(&TomlAdapter, content, key, false)
    }

    #[test]
    fn unset_removes_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = unset_doc(content, "port").unwrap();
        assert!(!result.contains("port"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_nested_key() {
        let content = "[database]\npool_size = 5\nurl = \"pg://\"\n";
        let result = unset_doc(content, "database.pool_size").unwrap();
        assert!(!result.contains("pool_size"));
        assert!(result.contains("url = \"pg://\""));
    }

    #[test]
    fn unset_nonexistent_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_doc(content, "missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_nonexistent_nested_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_doc(content, "database.missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_preserves_comments_on_other_keys() {
        let content = "port = 8080\n# The host address\nhost = \"localhost\"\n";
        let result = unset_doc(content, "port").unwrap();
        assert!(result.contains("# The host address"));
        assert!(result.contains("host = \"localhost\""));
        assert!(!result.contains("port"));
    }

    #[test]
    fn unset_value_removes_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\nhost = \"localhost\"\n").unwrap();

        let result = unset_value(&TomlAdapter, &path, "port", false).unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_value_missing_file_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = unset_value(&TomlAdapter, &path, "port", false).unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
    }
}
