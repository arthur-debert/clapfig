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
}
