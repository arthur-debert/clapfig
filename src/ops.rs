//! Config operations: template generation, key lookup, listing, and result types.
//!
//! Provides the logic behind `config list`, `config gen`, `config get`, and the
//! `ConfigResult` enum that callers use to display results.

use std::fmt;
use std::path::PathBuf;

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
pub fn generate_template<C: Config>() -> String {
    confique::toml::template::<C>(confique::toml::FormatOptions::default())
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
    let doc = lookup_doc(&C::META, key);

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

/// Walk confique's `Meta` tree to find the doc comment for a dotted key path.
fn lookup_doc(meta: &confique::meta::Meta, dotted_key: &str) -> Vec<String> {
    let segments: Vec<&str> = dotted_key.split('.').collect();
    lookup_doc_recursive(meta, &segments)
}

fn lookup_doc_recursive(meta: &confique::meta::Meta, segments: &[&str]) -> Vec<String> {
    if segments.is_empty() {
        return vec![];
    }

    for field in meta.fields {
        if field.name == segments[0] {
            if segments.len() == 1 {
                return field.doc.iter().map(|s| s.to_string()).collect();
            }
            if let confique::meta::FieldKind::Nested { meta: nested, .. } = &field.kind {
                return lookup_doc_recursive(nested, &segments[1..]);
            }
        }
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;

    fn test_config() -> TestConfig {
        TestConfig::builder().load().unwrap()
    }

    #[test]
    fn generate_template_contains_keys() {
        let template = generate_template::<TestConfig>();
        assert!(template.contains("host"));
        assert!(template.contains("port"));
        assert!(template.contains("database"));
        assert!(template.contains("pool_size"));
    }

    #[test]
    fn generate_template_contains_doc_comments() {
        let template = generate_template::<TestConfig>();
        assert!(template.contains("application host"));
        assert!(template.contains("port number"));
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
}
