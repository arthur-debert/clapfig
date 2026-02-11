//! Convert dotted-key CLI overrides into a nested `toml::Table`.
//!
//! Each `("database.url", Value)` pair is expanded into the nested table structure
//! needed for deep-merge with other config layers.

use toml::{Table, Value};

/// Convert dotted-key overrides into a nested `toml::Table`.
///
/// `("database.url", Value::String("pg://"))` becomes `{database = {url = "pg://"}}`
///
/// If multiple entries target the same key, the last one wins.
pub fn overrides_to_table(entries: &[(String, Value)]) -> Table {
    let mut table = Table::new();
    for (dotted_key, value) in entries {
        set_nested(&mut table, dotted_key, value.clone());
    }
    table
}

fn set_nested(table: &mut Table, dotted_key: &str, value: Value) {
    let segments: Vec<&str> = dotted_key.split('.').collect();
    let mut current = table;

    for segment in &segments[..segments.len() - 1] {
        current = current
            .entry(*segment)
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .expect("clapfig: override path conflict — intermediate key is not a table");
    }

    let leaf = segments.last().unwrap();
    current.insert(leaf.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn flat_key() {
        let table = overrides_to_table(&entries(&[("host", Value::String("0.0.0.0".into()))]));
        assert_eq!(table["host"].as_str().unwrap(), "0.0.0.0");
    }

    #[test]
    fn nested_key() {
        let table =
            overrides_to_table(&entries(&[("database.url", Value::String("pg://".into()))]));
        let db = table["database"].as_table().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "pg://");
    }

    #[test]
    fn deep_nesting() {
        let table = overrides_to_table(&entries(&[("a.b.c.d", Value::Integer(42))]));
        assert_eq!(table["a"]["b"]["c"]["d"].as_integer().unwrap(), 42);
    }

    #[test]
    fn multiple_entries_different_branches() {
        let table = overrides_to_table(&entries(&[
            ("host", Value::String("x".into())),
            ("database.url", Value::String("pg://".into())),
            ("database.pool_size", Value::Integer(20)),
        ]));
        assert_eq!(table["host"].as_str().unwrap(), "x");
        let db = table["database"].as_table().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "pg://");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
    }

    #[test]
    fn empty_list_empty_table() {
        let table = overrides_to_table(&[]);
        assert!(table.is_empty());
    }

    #[test]
    fn last_entry_wins_for_same_key() {
        let table = overrides_to_table(&entries(&[
            ("port", Value::Integer(3000)),
            ("port", Value::Integer(5000)),
        ]));
        assert_eq!(table["port"].as_integer().unwrap(), 5000);
    }
}
