//! Config persistence: patch values into TOML files while preserving formatting.
//!
//! Uses `toml_edit` for comment-preserving edits. When no file exists yet,
//! starts from the generated template so the new file includes doc comments.
//! Creates parent directories as needed.

use std::path::Path;

use confique::Config;
use serde::Deserialize;

use crate::error::ClapfigError;
use crate::ops::ConfigResult;

/// Pure function: patch a TOML document string, setting `key` to `raw_value`.
///
/// If `content` is `None` (file doesn't exist yet), starts from the generated template.
/// Uses `toml_edit` to preserve existing comments and formatting.
///
/// Returns the modified document string.
pub fn set_in_document<C: Config>(
    content: Option<&str>,
    key: &str,
    raw_value: &str,
) -> Result<String, ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
    // Validate key is known to the config schema
    let valid_keys = crate::overrides::valid_keys(crate::spec::SchemaRef::from_meta(&C::META));
    if !valid_keys.contains(key) {
        return Err(ClapfigError::KeyNotFound(key.into()));
    }

    // Validate value is compatible with the field's type by round-trip
    // deserializing a minimal table into C::Layer (all-optional fields).
    let check_value = parse_toml_value(raw_value);
    let check_table = crate::overrides::overrides_to_table(&[(key.to_string(), check_value)]);
    let _: C::Layer =
        toml::Value::Table(check_table)
            .try_into()
            .map_err(|e: toml::de::Error| ClapfigError::InvalidValue {
                key: key.into(),
                reason: e.to_string(),
            })?;

    let base = match content {
        Some(c) => c.to_string(),
        None => {
            // Start from template or empty
            // `config set` doesn't currently know about normalize_keys, so
            // the seeded template stays snake_case to match today's behavior.
            // Wiring kebab into the set/unset path (key validation + seed)
            // is a sibling fix tracked separately.
            let template = crate::ops::generate_template::<C>(false);
            if template.trim().is_empty() {
                String::new()
            } else {
                template
            }
        }
    };

    let mut doc: toml_edit::DocumentMut =
        base.parse()
            .map_err(|e: toml_edit::TomlError| ClapfigError::InvalidValue {
                key: key.into(),
                reason: e.to_string(),
            })?;

    let parsed_value = parse_toml_edit_value(raw_value);
    write_at_dotted_path(&mut doc, key, parsed_value)?;
    Ok(doc.to_string())
}

/// Runtime-path analogue of [`set_in_document`]. Validates the key against
/// an owned [`Schema`](crate::runtime::Schema) and the value against the
/// leaf's declared [`LeafType`](crate::runtime::LeafType) — same fail-fast
/// invariants as the static path, just driven from a runtime schema.
pub fn set_in_document_runtime(
    schema: &crate::runtime::Schema,
    content: Option<&str>,
    key: &str,
    raw_value: &str,
) -> Result<String, ClapfigError> {
    let valid_keys = crate::overrides::valid_keys(crate::spec::SchemaRef::from_dynamic(schema));
    if !valid_keys.contains(key) {
        return Err(ClapfigError::KeyNotFound(key.into()));
    }

    let check_value = parse_toml_value(raw_value);
    if let Some(leaf_ty) = lookup_leaf_type(schema, key) {
        leaf_ty
            .check(&check_value)
            .map_err(|reason| ClapfigError::InvalidValue {
                key: key.into(),
                reason,
            })?;
    }

    let base = match content {
        Some(c) => c.to_string(),
        None => crate::ops::generate_template_from_runtime(schema, false),
    };
    let base = if base.trim().is_empty() {
        String::new()
    } else {
        base
    };

    let mut doc: toml_edit::DocumentMut =
        base.parse()
            .map_err(|e: toml_edit::TomlError| ClapfigError::InvalidValue {
                key: key.into(),
                reason: e.to_string(),
            })?;

    let parsed_value = parse_toml_edit_value(raw_value);
    write_at_dotted_path(&mut doc, key, parsed_value)?;
    Ok(doc.to_string())
}

/// Walk a dotted path through `doc`, creating intermediate tables when
/// missing, and assign `value` at the leaf. Returns
/// [`ClapfigError::InvalidValue`] when an existing intermediate is a
/// non-table scalar — `toml_edit`'s `IndexMut` would panic on that case,
/// and the schema-time pre-checks only validate the schema, not the
/// shape of an existing on-disk file (`config set database.url x` with
/// `database = "string"` already in the file).
fn write_at_dotted_path(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: toml_edit::Value,
) -> Result<(), ClapfigError> {
    let segments: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = segments
        .split_last()
        .expect("split('.') always yields at least one segment");
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for segment in parents {
        if current.get(segment).is_none() {
            // current must be table-like to insert a new section here;
            // otherwise the existing file has a scalar where we need a
            // table.
            if current.as_table_like_mut().is_none() {
                return Err(ClapfigError::InvalidValue {
                    key: key.into(),
                    reason: format!(
                        "path conflict: existing file has a non-table value at the path before '{segment}'"
                    ),
                });
            }
            current[*segment] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        // After the (possibly-created) child, navigate into it. The child
        // might be a scalar from a pre-existing file; guard before
        // indexing.
        let next = current
            .get_mut(*segment)
            .expect("just confirmed or inserted above");
        if next.as_table_like().is_none() {
            return Err(ClapfigError::InvalidValue {
                key: key.into(),
                reason: format!(
                    "path conflict: existing file has a non-table value at '{segment}'"
                ),
            });
        }
        current = next;
    }
    if current.as_table_like_mut().is_none() {
        return Err(ClapfigError::InvalidValue {
            key: key.into(),
            reason: "path conflict: leaf parent is not a table".into(),
        });
    }
    current[*leaf] = toml_edit::value(value);
    Ok(())
}

/// Runtime-path wrapper around `set_in_document_runtime` with file I/O.
pub fn persist_value_runtime(
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

    let new_content = set_in_document_runtime(schema, content.as_deref(), key, value)?;

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

/// I/O wrapper: reads file (if it exists), patches it, writes back.
/// Creates parent directories if needed.
pub fn persist_value<C: Config>(
    file_path: &Path,
    key: &str,
    value: &str,
) -> Result<ConfigResult, ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
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

    let new_content = set_in_document::<C>(content.as_deref(), key, value)?;

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

/// Pure function: remove a key from a TOML document string.
///
/// If the key doesn't exist, returns the document unchanged.
/// Navigates dotted key paths (e.g. `"database.pool_size"`).
/// Uses `toml_edit` to preserve existing comments and formatting.
///
/// Returns the modified document string.
pub fn unset_in_document(content: &str, key: &str) -> Result<String, ClapfigError> {
    let mut doc: toml_edit::DocumentMut =
        content
            .parse()
            .map_err(|e: toml_edit::TomlError| ClapfigError::InvalidValue {
                key: key.into(),
                reason: e.to_string(),
            })?;

    let segments: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = segments
        .split_last()
        .expect("split('.') always yields at least one segment");

    // Navigate to the parent, then remove the leaf.
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for segment in parents {
        match current.get_mut(segment) {
            Some(item) => current = item,
            None => return Ok(doc.to_string()), // parent doesn't exist, nothing to unset
        }
    }

    if let Some(table) = current.as_table_like_mut() {
        table.remove(leaf);
    }

    Ok(doc.to_string())
}

/// I/O wrapper: reads file, removes the key, writes back.
/// If the file doesn't exist, succeeds silently (nothing to unset).
pub fn unset_value(file_path: &Path, key: &str) -> Result<ConfigResult, ClapfigError> {
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

    let new_content = unset_in_document(&content, key)?;

    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    Ok(ConfigResult::ValueUnset { key: key.into() })
}

/// Parse a raw string value into a `toml::Value` with type heuristics.
///
/// Used for round-trip validation: build a `toml::Table` and deserialize into
/// `C::Layer` to catch type mismatches before persisting.
fn parse_toml_value(s: &str) -> toml::Value {
    if s.eq_ignore_ascii_case("true") {
        return toml::Value::Boolean(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return toml::Value::Boolean(false);
    }
    if let Ok(i) = s.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if s.contains('.')
        && let Ok(f) = s.parse::<f64>()
    {
        return toml::Value::Float(f);
    }
    toml::Value::String(s.to_string())
}

/// Parse a raw string value into a `toml_edit::Value` with type heuristics.
fn parse_toml_edit_value(s: &str) -> toml_edit::Value {
    if s.eq_ignore_ascii_case("true") {
        return toml_edit::value(true).into_value().unwrap();
    }
    if s.eq_ignore_ascii_case("false") {
        return toml_edit::value(false).into_value().unwrap();
    }
    if let Ok(i) = s.parse::<i64>() {
        return toml_edit::value(i).into_value().unwrap();
    }
    if s.contains('.')
        && let Ok(f) = s.parse::<f64>()
    {
        return toml_edit::value(f).into_value().unwrap();
    }
    toml_edit::value(s).into_value().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::{EnumConfig, TestConfig};
    use std::fs;
    use tempfile::TempDir;

    // --- validation tests ---

    #[test]
    fn set_rejects_unknown_key() {
        let result = set_in_document::<TestConfig>(Some(""), "nonexistent", "value");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn set_rejects_invalid_enum_value() {
        let result = set_in_document::<EnumConfig>(Some(""), "mode", "garbage");
        match result {
            Err(ClapfigError::InvalidValue { key, reason }) => {
                assert_eq!(key, "mode");
                assert!(
                    reason.contains("unknown variant"),
                    "expected 'unknown variant' in: {reason}"
                );
            }
            other => panic!("Expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_accepts_valid_enum_value() {
        let result = set_in_document::<EnumConfig>(Some(""), "mode", "fast");
        assert!(result.is_ok());
    }

    #[test]
    fn set_rejects_wrong_type() {
        let result = set_in_document::<TestConfig>(Some(""), "port", "not_a_number");
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
    }

    #[test]
    fn set_rejects_path_through_scalar() {
        // Existing file has `database` as a scalar string; `config set
        // database.url x` would dereference into a non-table item, which
        // pre-fix would panic via `toml_edit::Item::IndexMut`. The guard
        // turns it into a clean InvalidValue.
        let content = "database = \"oops\"\n";
        let result = set_in_document::<TestConfig>(Some(content), "database.url", "pg://x");
        match result {
            Err(ClapfigError::InvalidValue { key, reason }) => {
                assert_eq!(key, "database.url");
                assert!(reason.contains("path conflict"), "got: {reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn persist_rejects_invalid_enum_value() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_value::<EnumConfig>(&path, "mode", "garbage");
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
        // File should NOT have been created
        assert!(!path.exists());
    }

    #[test]
    fn set_existing_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = set_in_document::<TestConfig>(Some(content), "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn set_nested_key() {
        let content = "[database]\npool_size = 5\n";
        let result =
            set_in_document::<TestConfig>(Some(content), "database.pool_size", "20").unwrap();
        assert!(result.contains("pool_size = 20"));
    }

    #[test]
    fn set_new_key_in_existing_file() {
        let content = "port = 8080\n";
        let result = set_in_document::<TestConfig>(Some(content), "debug", "true").unwrap();
        assert!(result.contains("debug = true"));
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn set_creates_from_template_when_none() {
        let result = set_in_document::<TestConfig>(None, "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
    }

    #[test]
    fn preserves_comments() {
        let content = "# This is my config\nport = 8080\n# end\n";
        let result = set_in_document::<TestConfig>(Some(content), "port", "3000").unwrap();
        assert!(result.contains("# This is my config"));
        assert!(result.contains("port = 3000"));
    }

    #[test]
    fn value_parsing_integer() {
        let v = parse_toml_edit_value("42");
        assert!(v.is_integer());
    }

    #[test]
    fn value_parsing_bool() {
        let v = parse_toml_edit_value("true");
        assert!(v.is_bool());
    }

    #[test]
    fn value_parsing_string() {
        let v = parse_toml_edit_value("hello");
        assert!(v.is_str());
    }

    #[test]
    fn value_parsing_float() {
        let v = parse_toml_edit_value("1.5");
        assert!(v.is_float());
    }

    #[test]
    fn persist_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_value::<TestConfig>(&path, "port", "3000").unwrap();
        assert!(matches!(result, ConfigResult::ValueSet { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
    }

    #[test]
    fn persist_modifies_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\n").unwrap();

        persist_value::<TestConfig>(&path, "port", "3000").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
        assert!(!content.contains("8080"));
    }

    #[test]
    fn persist_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("dir").join("config.toml");

        persist_value::<TestConfig>(&path, "port", "3000").unwrap();
        assert!(path.exists());
    }

    // --- unset tests ---

    #[test]
    fn unset_removes_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = unset_in_document(content, "port").unwrap();
        assert!(!result.contains("port"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_nested_key() {
        let content = "[database]\npool_size = 5\nurl = \"pg://\"\n";
        let result = unset_in_document(content, "database.pool_size").unwrap();
        assert!(!result.contains("pool_size"));
        assert!(result.contains("url = \"pg://\""));
    }

    #[test]
    fn unset_nonexistent_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_in_document(content, "missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_nonexistent_nested_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_in_document(content, "database.missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_preserves_comments_on_other_keys() {
        let content = "port = 8080\n# The host address\nhost = \"localhost\"\n";
        let result = unset_in_document(content, "port").unwrap();
        assert!(result.contains("# The host address"));
        assert!(result.contains("host = \"localhost\""));
        assert!(!result.contains("port"));
    }

    #[test]
    fn unset_value_removes_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\nhost = \"localhost\"\n").unwrap();

        let result = unset_value(&path, "port").unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_value_missing_file_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = unset_value(&path, "port").unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
    }
}
