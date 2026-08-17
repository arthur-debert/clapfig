//! Config operations: template generation, key lookup, listing, and result types.
//!
//! Provides the logic behind `config list`, `config gen`, `config get`, and the
//! `ConfigResult` enum that callers use to display results.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::ClapfigError;
use crate::format::FormatAdapter;
use crate::value::{Map, Value};

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

/// Rewrite snake_case keys to kebab-case in a generated config template.
///
/// Walks the template line by line and rewrites:
/// - `[section]` / `[parent.section]` / `[[array]]` headers
/// - `key = value` lines
/// - `#key = value` commented-default lines (single or multi-hash prefix)
///
/// Doc-comment lines (a `#` followed by a space or end-of-line) and the
/// value portion of any line are left untouched. The disambiguation between
/// a doc comment and a commented-default line is the template convention
/// used by [`generate_template`]: `# <text>` is documentation,
/// `#key = ...` (no space after the hashes) is a commented key.
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
        // Template convention: hashes + whitespace (or EOL) = doc comment;
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

/// Template generator: renders the documented config template for a
/// schema through the given format adapter, then optionally applies the
/// kebab rewriter so the rendered keys match what a
/// `normalize_keys(true)` builder accepts.
///
/// The template body — doc comments, `# Allowed:` enum lines,
/// commented placeholders for defaultless leaves — is the adapter's
/// [`template`](crate::format::FormatAdapter::template) output; this
/// function is the routing seam every template consumer (`config gen`,
/// file seeding) goes through.
pub(crate) fn generate_template(
    adapter: &dyn FormatAdapter,
    schema: &crate::runtime::Schema,
    kebab: bool,
) -> Result<String, ClapfigError> {
    let out = adapter.template(schema)?;
    Ok(if kebab {
        rewrite_keys_to_kebab(&out)
    } else {
        out
    })
}

/// List entries from a single scope's config file (raw file content, not merged).
///
/// The file is parsed through `adapter` — the routing seam for scoped
/// reads. If the file does not exist, returns an empty listing.
pub(crate) fn list_scope_file(
    adapter: &dyn FormatAdapter,
    file_path: &Path,
) -> Result<ConfigResult, ClapfigError> {
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

    let parsed = adapter
        .parse(&content)
        .map_err(|e| ClapfigError::ParseError {
            path: file_path.to_path_buf(),
            source: Box::new(e),
            source_text: Some(std::sync::Arc::from(content.as_str())),
        })?;
    let table = match parsed {
        Value::Map(map) => map,
        other => {
            return Err(ClapfigError::InvalidValue {
                key: file_path.display().to_string(),
                reason: format!(
                    "config documents must be maps at the root, got {}",
                    other.type_str()
                ),
            });
        }
    };

    let mut entries = Vec::new();
    flatten_value_map(&table, "", &mut entries);

    Ok(ConfigResult::Listing { entries })
}

/// Recursively flatten a value map into dotted key-value pairs.
fn flatten_value_map(table: &Map, prefix: &str, entries: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Map(t) => flatten_value_map(t, &full_key, entries),
            _ => entries.push((full_key, format_value(value))),
        }
    }
}

/// Navigate a value [`Map`] by dotted key path (e.g. `"database.url"`).
pub(crate) fn table_get<'a>(table: &'a Map, dotted_key: &str) -> Option<&'a Value> {
    let (path, leaf) = match dotted_key.rsplit_once('.') {
        Some((p, l)) => (Some(p), l),
        None => (None, dotted_key),
    };

    let tbl = match path {
        Some(path) => {
            let mut current = table;
            for segment in path.split('.') {
                current = current.get(segment)?.as_map()?;
            }
            current
        }
        None => table,
    };

    tbl.get(leaf)
}

/// Format a config value for display in listings and `config get` output.
fn format_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(_) | Value::Map(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;
    use crate::format::TomlAdapter;

    fn generate_template_from_runtime(schema: &crate::runtime::Schema, kebab: bool) -> String {
        generate_template(&TomlAdapter, schema, kebab).unwrap()
    }

    #[test]
    fn generate_template_contains_keys() {
        let template = generate_template_from_runtime(&test_schema(), false);
        assert!(template.contains("host"));
        assert!(template.contains("port"));
        assert!(template.contains("database"));
        assert!(template.contains("pool_size"));
    }

    #[test]
    fn generate_template_contains_doc_comments() {
        let template = generate_template_from_runtime(&test_schema(), false);
        assert!(template.contains("application host"));
        assert!(template.contains("port number"));
    }

    #[test]
    fn generate_template_kebab_rewrites_snake_keys() {
        let template = generate_template_from_runtime(&test_schema(), true);
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
        // as keys) should not be rewritten. The fixture's docs include
        // "Connection pool size."—lowercase plain English—but we also want
        // the structural guarantee that `# ` lines pass through verbatim.
        let template = generate_template_from_runtime(&test_schema(), true);
        assert!(template.contains("Connection pool size."));
    }

    #[test]
    fn generate_template_kebab_off_is_default_behavior() {
        // Sanity: with the flag off, snake keys pass through untouched
        // (kebab path is opt-in).
        let raw = generate_template_from_runtime(&test_schema(), false);
        assert!(raw.contains("pool_size"));
        assert!(!raw.contains("pool-size"));
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
        // `#` followed by a space is a doc comment in the template
        // convention — any `_` in prose must survive the rewriter untouched.
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
    fn table_get_flat() {
        let table = crate::fixtures::test::parse_toml("port = 8080");
        let val = table_get(&table, "port").unwrap();
        assert_eq!(val.as_integer().unwrap(), 8080);
    }

    #[test]
    fn table_get_nested() {
        let table = crate::fixtures::test::parse_toml("[database]\npool_size = 5");
        let val = table_get(&table, "database.pool_size").unwrap();
        assert_eq!(val.as_integer().unwrap(), 5);
    }

    #[test]
    fn table_get_missing() {
        let table = crate::fixtures::test::parse_toml("port = 8080");
        assert!(table_get(&table, "nope").is_none());
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

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
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

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
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

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
        match result {
            ConfigResult::Listing { entries } => assert!(entries.is_empty()),
            other => panic!("Expected empty Listing, got {other:?}"),
        }
    }
}
