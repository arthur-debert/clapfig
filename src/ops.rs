//! Config operations: template generation, key lookup, listing, and result types.
//!
//! Provides the logic behind `config list`, `config gen`, `config get`, and the
//! `ConfigResult` enum that callers use to display results.

use std::fmt;
use std::path::{Path, PathBuf};

use confique::Config;
use serde::Serialize;

use crate::error::ClapfigError;

/// Result of a config operation. Returned to the caller for display.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigResult {
    /// A generated TOML template string.
    Template(String),
    /// Confirmation that a template was written to a file.
    TemplateWritten { path: PathBuf },
    /// A generated JSON Schema document (already serialized).
    Schema(String),
    /// Confirmation that a JSON Schema document was written to a file.
    SchemaWritten { path: PathBuf },
    /// A key's resolved value and its doc comment.
    KeyValue {
        key: String,
        value: String,
        doc: Vec<String>,
    },
    /// Confirmation that a value was persisted.
    ValueSet { key: String, value: String },
    /// Confirmation that a value was removed.
    ValueUnset { key: String },
    /// All resolved configuration key-value pairs.
    Listing { entries: Vec<(String, String)> },
}

impl fmt::Display for ConfigResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigResult::Template(t) => write!(f, "{t}"),
            ConfigResult::TemplateWritten { path } => {
                write!(f, "Config template written to {}", path.display())
            }
            ConfigResult::Schema(s) => write!(f, "{s}"),
            ConfigResult::SchemaWritten { path } => {
                write!(f, "Config schema written to {}", path.display())
            }
            ConfigResult::KeyValue { key, value, doc } => {
                for line in doc {
                    writeln!(f, "# {line}")?;
                }
                write!(f, "{key} = {value}")
            }
            ConfigResult::ValueSet { key, value } => write!(f, "Set {key} = {value}"),
            ConfigResult::ValueUnset { key } => write!(f, "Unset {key}"),
            ConfigResult::Listing { entries } => {
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{key} = {value}")?;
                }
                Ok(())
            }
        }
    }
}

/// Generate a commented TOML template from the config struct's doc comments.
///
/// When `kebab` is `true`, snake_case field names in the template's keys and
/// section headers are rewritten to kebab-case (`pool_size` → `pool-size`)
/// so the template matches what users will actually type when
/// [`.normalize_keys(true)`](crate::ClapfigBuilder::normalize_keys) is in
/// effect. Doc comments and values are never touched.
pub fn generate_template<C: Config>(kebab: bool) -> String {
    let raw = confique::toml::template::<C>(confique::toml::FormatOptions::default());
    if kebab {
        rewrite_keys_to_kebab(&raw)
    } else {
        raw
    }
}

/// Rewrite snake_case keys to kebab-case in a confique TOML template.
///
/// Walks the template line by line and rewrites:
/// - `[section]` / `[parent.section]` / `[[array]]` headers
/// - `key = value` lines
/// - `#key = value` commented-default lines (single or multi-hash prefix)
///
/// Doc-comment lines (a `#` followed by a space or end-of-line) and the
/// value portion of any line are left untouched. The disambiguation between
/// a doc comment and a commented-default line is the same convention
/// confique itself uses: `# <text>` is documentation, `#key = ...` (no
/// space after the hashes) is a commented key.
fn rewrite_keys_to_kebab(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let ends_with_newline = template.ends_with('\n');

    for (i, line) in template.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&rewrite_template_line(line));
    }
    if ends_with_newline {
        out.push('\n');
    }
    out
}

fn rewrite_template_line(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let stripped = &line[indent_len..];

    // [[array.of.tables]] — must be checked before [section] since the
    // shorter prefix `[` is a substring of `[[`.
    if let Some(rest) = stripped.strip_prefix("[[")
        && let Some(end) = rest.find("]]")
    {
        let name = rest[..end].trim();
        let tail = &rest[end + 2..];
        return format!("{indent}[[{}]]{tail}", swap_underscores_to_dashes(name));
    }

    // [section] / [parent.section]
    if let Some(rest) = stripped.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let name = rest[..end].trim();
        let tail = &rest[end + 1..];
        return format!("{indent}[{}]{tail}", swap_underscores_to_dashes(name));
    }

    // Commented-out key (#key = ...) vs. doc comment (# <text>).
    let (hashes, body) = if stripped.starts_with('#') {
        let count = stripped.bytes().take_while(|&b| b == b'#').count();
        let after = &stripped[count..];
        // confique's convention: hashes + whitespace (or EOL) = doc comment;
        // hashes + bareword = commented-out default. Only rewrite the latter.
        if after.is_empty() || after.starts_with(|c: char| c.is_whitespace()) {
            return line.to_string();
        }
        (&stripped[..count], after)
    } else {
        ("", stripped)
    };

    // Plain or commented "key = value" line.
    if let Some(eq_idx) = body.find('=') {
        let key_part = &body[..eq_idx];
        let key_trimmed = key_part.trim();
        if is_bareword_dotted_key(key_trimmed) {
            // Preserve whitespace around the key exactly.
            let leading_ws_in_key = &key_part[..key_part.len() - key_part.trim_start().len()];
            let trailing_ws_in_key = &key_part[leading_ws_in_key.len() + key_trimmed.len()..];
            let rest = &body[eq_idx..];
            return format!(
                "{indent}{hashes}{leading_ws_in_key}{}{trailing_ws_in_key}{rest}",
                swap_underscores_to_dashes(key_trimmed),
            );
        }
    }

    line.to_string()
}

fn is_bareword_dotted_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn swap_underscores_to_dashes(dotted: &str) -> String {
    if !dotted.contains('_') {
        return dotted.to_string();
    }
    dotted.replace('_', "-")
}

/// Generate a JSON Schema document (pretty-printed) describing the config struct.
///
/// Delegates to [`crate::schema::generate_schema`] and serializes the result.
/// Serialization of a `serde_json::Value` is infallible for the shapes this
/// module produces, so we propagate any panic rather than masking it with a
/// bogus "{}" payload.
pub fn generate_schema_string<C: Config>() -> String {
    let value = crate::schema::generate_schema::<C>();
    serde_json::to_string_pretty(&value).expect("serde_json::Value serialization is infallible")
}

/// Get a config value by dotted key, including its doc comment.
pub fn get_value<C: Config + Serialize>(
    config: &C,
    key: &str,
) -> Result<ConfigResult, ClapfigError> {
    let toml_value = toml::Value::try_from(config).map_err(|e| ClapfigError::InvalidValue {
        key: key.into(),
        reason: e.to_string(),
    })?;

    let table = toml_value
        .as_table()
        .ok_or_else(|| ClapfigError::InvalidValue {
            key: key.into(),
            reason: "config did not serialize to a table".into(),
        })?;

    let value = table_get(table, key).ok_or_else(|| ClapfigError::KeyNotFound(key.into()))?;

    let value_str = format_value(value);
    let doc = crate::meta::doc_for::<C>(key).unwrap_or_default();

    Ok(ConfigResult::KeyValue {
        key: key.into(),
        value: value_str,
        doc,
    })
}

/// List all resolved config values as flattened dotted key-value pairs.
pub fn list_values<C: Config + Serialize>(config: &C) -> Result<ConfigResult, ClapfigError> {
    let pairs = crate::flatten::flatten(config).map_err(|e| ClapfigError::InvalidValue {
        key: "<list>".into(),
        reason: e.to_string(),
    })?;

    let entries: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(key, value)| {
            let display = match value {
                Some(v) => format_value(&v),
                None => "<not set>".to_string(),
            };
            (key, display)
        })
        .collect();

    Ok(ConfigResult::Listing { entries })
}

/// List entries from a single scope's config file (raw file content, not merged).
///
/// If the file does not exist, returns an empty listing.
pub fn list_scope_file(file_path: &Path) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigResult::Listing {
                entries: Vec::new(),
            });
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let table: toml::Table =
        content
            .parse()
            .map_err(|e: toml::de::Error| ClapfigError::ParseError {
                path: file_path.to_path_buf(),
                source: Box::new(e),
                source_text: Some(std::sync::Arc::from(content.as_str())),
            })?;

    let mut entries = Vec::new();
    flatten_toml_table(&table, "", &mut entries);

    Ok(ConfigResult::Listing { entries })
}

/// Get a value from a single scope's config file by dotted key.
///
/// Returns the raw value from the file, plus doc comments from the config struct's
/// metadata. Returns `KeyNotFound` if the key is not present in the file.
pub fn get_scope_value<C: Config>(
    file_path: &Path,
    key: &str,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClapfigError::KeyNotFound(key.into()));
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let table: toml::Table =
        content
            .parse()
            .map_err(|e: toml::de::Error| ClapfigError::ParseError {
                path: file_path.to_path_buf(),
                source: Box::new(e),
                source_text: Some(std::sync::Arc::from(content.as_str())),
            })?;

    let value = table_get(&table, key).ok_or_else(|| ClapfigError::KeyNotFound(key.into()))?;
    let value_str = format_value(value);
    let doc = crate::meta::doc_for::<C>(key).unwrap_or_default();

    Ok(ConfigResult::KeyValue {
        key: key.into(),
        value: value_str,
        doc,
    })
}

/// Recursively flatten a TOML table into dotted key-value pairs.
fn flatten_toml_table(table: &toml::Table, prefix: &str, entries: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(t) => flatten_toml_table(t, &full_key, entries),
            _ => entries.push((full_key, format_value(value))),
        }
    }
}

/// Navigate a `toml::Table` by dotted key path (e.g. `"database.url"`).
pub fn table_get<'a>(table: &'a toml::Table, dotted_key: &str) -> Option<&'a toml::Value> {
    let (path, leaf) = match dotted_key.rsplit_once('.') {
        Some((p, l)) => (Some(p), l),
        None => (None, dotted_key),
    };

    let tbl = match path {
        Some(path) => {
            let mut current = table;
            for segment in path.split('.') {
                current = current.get(segment)?.as_table()?;
            }
            current
        }
        None => table,
    };

    tbl.get(leaf)
}

/// Format a TOML value for display.
fn format_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Array(a) => toml::to_string(&a).unwrap_or_else(|_| format!("{a:?}")),
        toml::Value::Table(t) => toml::to_string(&t).unwrap_or_else(|_| format!("{t:?}")),
        _ => format!("{value:?}"),
    }
}

// Doc lookup lives in `crate::meta::doc_for` — see ops.rs callers above
// for the `unwrap_or_default()` adapter that produces the same `Vec<String>`
// shape the old internal helper used.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;

    fn test_config() -> TestConfig {
        TestConfig::builder().load().unwrap()
    }

    #[test]
    fn generate_template_contains_keys() {
        let template = generate_template::<TestConfig>(false);
        assert!(template.contains("host"));
        assert!(template.contains("port"));
        assert!(template.contains("database"));
        assert!(template.contains("pool_size"));
    }

    #[test]
    fn generate_template_contains_doc_comments() {
        let template = generate_template::<TestConfig>(false);
        assert!(template.contains("application host"));
        assert!(template.contains("port number"));
    }

    #[test]
    fn generate_template_kebab_rewrites_snake_keys() {
        let template = generate_template::<TestConfig>(true);
        // The nested `pool_size` field should be emitted as `pool-size`.
        assert!(
            template.contains("pool-size"),
            "expected kebab key in template:\n{template}"
        );
        assert!(
            !template.contains("pool_size"),
            "expected no snake leak in template:\n{template}"
        );
    }

    #[test]
    fn generate_template_kebab_preserves_doc_comments() {
        // Doc comments that happen to mention the snake form (in prose, not
        // as keys) should not be rewritten. TestConfig's docs include
        // "Connection pool size."—lowercase plain English—but we also want
        // the structural guarantee that `# ` lines pass through verbatim.
        let template = generate_template::<TestConfig>(true);
        assert!(template.contains("Connection pool size."));
    }

    #[test]
    fn generate_template_kebab_off_is_default_behavior() {
        // Sanity: with the flag off, output is byte-identical to the bare
        // confique template (kebab path is opt-in).
        let raw = generate_template::<TestConfig>(false);
        let bare = confique::toml::template::<TestConfig>(confique::toml::FormatOptions::default());
        assert_eq!(raw, bare);
    }

    // -- rewrite_keys_to_kebab unit tests -----------------------------------

    #[test]
    fn rewriter_handles_section_headers() {
        let input = "[my_section]\n[parent.my_child]\n";
        let out = rewrite_keys_to_kebab(input);
        assert!(out.contains("[my-section]"));
        assert!(out.contains("[parent.my-child]"));
    }

    #[test]
    fn rewriter_handles_array_of_tables_headers() {
        let input = "[[my_list]]\n";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, "[[my-list]]\n");
    }

    #[test]
    fn rewriter_handles_commented_default_keys() {
        let input = "#pool_size = 10\n";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, "#pool-size = 10\n");
    }

    #[test]
    fn rewriter_handles_uncommented_keys() {
        let input = "pool_size = 10\n";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, "pool-size = 10\n");
    }

    #[test]
    fn rewriter_skips_doc_comments() {
        // `#` followed by a space is a doc comment in confique's convention —
        // any `_` in prose must survive the rewriter untouched.
        let input = "# Set pool_size to a positive integer.\n";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, input);
    }

    #[test]
    fn rewriter_leaves_value_underscores_alone() {
        // Underscores in the value portion (e.g. inside string defaults)
        // must not be touched — only the key gets rewritten.
        let input = r#"db_url = "postgres://my_user@host""#.to_string() + "\n";
        let out = rewrite_keys_to_kebab(&input);
        assert!(out.contains("db-url = "));
        assert!(out.contains(r#""postgres://my_user@host""#));
    }

    #[test]
    fn rewriter_preserves_blank_lines() {
        let input = "key_one = 1\n\nkey_two = 2\n";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, "key-one = 1\n\nkey-two = 2\n");
    }

    #[test]
    fn rewriter_preserves_trailing_newline_absence() {
        // If the original lacks a trailing newline, the rewritten output
        // shouldn't sprout one.
        let input = "pool_size = 10";
        let out = rewrite_keys_to_kebab(input);
        assert_eq!(out, "pool-size = 10");
    }

    #[test]
    fn get_flat_key() {
        let config = test_config();
        let result = get_value::<TestConfig>(&config, "port").unwrap();
        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "8080"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn get_nested_key() {
        let config = test_config();
        let result = get_value::<TestConfig>(&config, "database.pool_size").unwrap();
        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "5"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn get_nonexistent_key() {
        let config = test_config();
        let result = get_value::<TestConfig>(&config, "nonexistent");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn get_includes_doc() {
        let config = test_config();
        let result = get_value::<TestConfig>(&config, "host").unwrap();
        match result {
            ConfigResult::KeyValue { doc, .. } => {
                let doc_text = doc.join(" ");
                assert!(
                    doc_text.contains("host"),
                    "doc should mention host: {doc_text}"
                );
            }
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn get_nested_doc() {
        let config = test_config();
        let result = get_value::<TestConfig>(&config, "database.pool_size").unwrap();
        match result {
            ConfigResult::KeyValue { doc, .. } => {
                let doc_text = doc.join(" ");
                assert!(
                    doc_text.contains("pool size") || doc_text.contains("Connection pool"),
                    "doc should mention pool: {doc_text}"
                );
            }
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn table_get_flat() {
        let table: toml::Table = toml::from_str("port = 8080").unwrap();
        let val = table_get(&table, "port").unwrap();
        assert_eq!(val.as_integer().unwrap(), 8080);
    }

    #[test]
    fn table_get_nested() {
        let table: toml::Table = toml::from_str("[database]\npool_size = 5").unwrap();
        let val = table_get(&table, "database.pool_size").unwrap();
        assert_eq!(val.as_integer().unwrap(), 5);
    }

    #[test]
    fn table_get_missing() {
        let table: toml::Table = toml::from_str("port = 8080").unwrap();
        assert!(table_get(&table, "nope").is_none());
    }

    #[test]
    fn list_values_includes_all_keys() {
        let config = test_config();
        let result = list_values::<TestConfig>(&config).unwrap();
        match result {
            ConfigResult::Listing { entries } => {
                let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
                assert!(keys.contains(&"host"));
                assert!(keys.contains(&"port"));
                assert!(keys.contains(&"debug"));
                assert!(keys.contains(&"database.url"));
                assert!(keys.contains(&"database.pool_size"));
                assert_eq!(entries.len(), 5);
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_values_shows_not_set_for_none() {
        let config = test_config();
        let result = list_values::<TestConfig>(&config).unwrap();
        match result {
            ConfigResult::Listing { entries } => {
                let db_url = entries.iter().find(|(k, _)| k == "database.url").unwrap();
                assert_eq!(db_url.1, "<not set>");
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_values_formats_correctly() {
        let config = test_config();
        let result = list_values::<TestConfig>(&config).unwrap();
        match result {
            ConfigResult::Listing { entries } => {
                let port = entries.iter().find(|(k, _)| k == "port").unwrap();
                assert_eq!(port.1, "8080");
                let host = entries.iter().find(|(k, _)| k == "host").unwrap();
                assert_eq!(host.1, "localhost");
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn listing_display_format() {
        let result = ConfigResult::Listing {
            entries: vec![
                ("host".into(), "localhost".into()),
                ("port".into(), "8080".into()),
            ],
        };
        let display = format!("{result}");
        assert_eq!(display, "host = localhost\nport = 8080");
    }

    // --- scope file operations ---

    #[test]
    fn list_scope_file_returns_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 3000\nhost = \"localhost\"\n").unwrap();

        let result = list_scope_file(&path).unwrap();
        match result {
            ConfigResult::Listing { entries } => {
                assert_eq!(entries.len(), 2);
                assert!(entries.contains(&("host".into(), "localhost".into())));
                assert!(entries.contains(&("port".into(), "3000".into())));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_scope_file_nested() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[database]\npool_size = 10\nurl = \"pg://\"\n").unwrap();

        let result = list_scope_file(&path).unwrap();
        match result {
            ConfigResult::Listing { entries } => {
                assert!(entries.contains(&("database.pool_size".into(), "10".into())));
                assert!(entries.contains(&("database.url".into(), "pg://".into())));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_scope_file_missing_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = list_scope_file(&path).unwrap();
        match result {
            ConfigResult::Listing { entries } => assert!(entries.is_empty()),
            other => panic!("Expected empty Listing, got {other:?}"),
        }
    }

    #[test]
    fn get_scope_value_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 3000\n").unwrap();

        let result = get_scope_value::<TestConfig>(&path, "port").unwrap();
        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "3000"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn get_scope_value_nested() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[database]\npool_size = 20\n").unwrap();

        let result = get_scope_value::<TestConfig>(&path, "database.pool_size").unwrap();
        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "20"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn get_scope_value_not_found() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 3000\n").unwrap();

        let result = get_scope_value::<TestConfig>(&path, "missing");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn get_scope_value_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = get_scope_value::<TestConfig>(&path, "port");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound(_))));
    }

    #[test]
    fn get_scope_value_includes_doc() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "host = \"myhost\"\n").unwrap();

        let result = get_scope_value::<TestConfig>(&path, "host").unwrap();
        match result {
            ConfigResult::KeyValue { doc, .. } => {
                let doc_text = doc.join(" ");
                assert!(
                    doc_text.contains("host"),
                    "doc should mention host: {doc_text}"
                );
            }
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }
}
