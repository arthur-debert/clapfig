//! Config persistence: patch values into config files while preserving
//! formatting.
//!
//! All source-text mechanics route through the format adapter contract
//! ([`FormatAdapter::edit`]) — for TOML that is lossless
//! comment-preserving editing. This module owns the format-agnostic half:
//! key/type validation against the schema, template seeding for missing
//! files, and the file I/O around each edit.

use std::path::Path;

use crate::error::ClapfigError;
use crate::format::{ConfigPath, FileEdit, FormatAdapter};
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
/// If `content` is `None` (file doesn't exist yet), starts from the
/// adapter's generated template so the new file carries doc comments.
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
) -> Result<String, ClapfigError> {
    let valid_keys = crate::overrides::valid_keys(crate::spec::SchemaRef::from_dynamic(schema));
    if !valid_keys.contains(key) {
        return Err(ClapfigError::KeyNotFound(key.into()));
    }

    let mut value = parse_raw_value(raw_value);
    if let Some(leaf_ty) = lookup_leaf_type(schema, key) {
        crate::runtime_spec::coerce_datetime_value(&mut value, leaf_ty);
        leaf_ty
            .check(&value)
            .map_err(|reason| ClapfigError::InvalidValue {
                key: key.into(),
                reason,
            })?;
    }

    let base = match content {
        Some(c) => c.to_string(),
        None => crate::ops::generate_template(adapter, schema, false)?,
    };
    let base = if base.trim().is_empty() {
        String::new()
    } else {
        base
    };

    let path = dotted_config_path(key);
    adapter
        .edit(
            &base,
            FileEdit::Set {
                path: &path,
                value: &value,
            },
        )
        .map_err(ClapfigError::from)
}

/// Wrapper around [`set_in_document_runtime`] with file I/O: reads the file
/// (if it exists), patches it, writes back. Creates parent directories if
/// needed.
pub fn persist_value_runtime(
    adapter: &dyn FormatAdapter,
    schema: &crate::runtime::Schema,
    file_path: &Path,
    key: &str,
    value: &str,
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

    let new_content = set_in_document_runtime(adapter, schema, content.as_deref(), key, value)?;

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
/// Navigates dotted key paths (e.g. `"database.pool_size"`). Comment
/// preservation is per the adapter's declared edit capability; adapter
/// failures propagate as [`ClapfigError::Format`].
///
/// Returns the modified document string.
pub fn unset_in_document(
    adapter: &dyn FormatAdapter,
    content: &str,
    key: &str,
) -> Result<String, ClapfigError> {
    let path = dotted_config_path(key);
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

    let new_content = unset_in_document(adapter, &content, key)?;

    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    Ok(ConfigResult::ValueUnset { key: key.into() })
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
        set_in_document_runtime(&TomlAdapter, &test_schema(), content, key, value)
    }

    fn persist_value(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> Result<ConfigResult, ClapfigError> {
        persist_value_runtime(&TomlAdapter, &test_schema(), path, key, value)
    }

    // --- validation tests ---

    #[test]
    fn set_rejects_unknown_key() {
        let result = set_in_document(Some(""), "nonexistent", "value");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn set_rejects_invalid_enum_value() {
        let result =
            set_in_document_runtime(&TomlAdapter, &enum_schema(), Some(""), "mode", "garbage");
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
        let result =
            set_in_document_runtime(&TomlAdapter, &enum_schema(), Some(""), "mode", "fast");
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

        let result = persist_value_runtime(&TomlAdapter, &enum_schema(), &path, "mode", "garbage");
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
        )
        .unwrap();
        assert!(
            result.contains("stamp = 2024-01-02T03:04:05Z"),
            "datetime must persist unquoted (typed), got: {result}"
        );

        let err = set_in_document_runtime(&TomlAdapter, &schema, Some(""), "stamp", "not-a-date")
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

    // --- unset tests ---

    fn unset_doc(content: &str, key: &str) -> Result<String, ClapfigError> {
        unset_in_document(&TomlAdapter, content, key)
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

        let result = unset_value(&TomlAdapter, &path, "port").unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_value_missing_file_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = unset_value(&TomlAdapter, &path, "port").unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
    }
}
