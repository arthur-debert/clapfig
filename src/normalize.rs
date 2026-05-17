//! Kebab-case → snake_case key normalization.
//!
//! When [`ClapfigBuilder::normalize_keys`](crate::ClapfigBuilder::normalize_keys)
//! is enabled, every key crossing the boundary into clapfig — TOML file keys,
//! CLI override key strings, URL query parameter keys — has its `-` characters
//! rewritten to `_` before deserialization, validation, and merging. The user
//! can then write `pool-size` in a config file (or `--set pool-size=10` on the
//! CLI) and it maps to a Rust field named `pool_size`.
//!
//! The transform is unconditional once enabled: clapfig does not try to detect
//! whether a particular key "should" be normalized. The motivating principle is
//! that key strings supplied by the user are never used directly — they go
//! through this normalization step first.

use toml::{Table, Value};

/// Replace every `-` with `_` in a single key string.
///
/// Used for dotted CLI/URL override paths (`"database.pool-size"`
/// → `"database.pool_size"`) — `.` segment separators are preserved because
/// only `-` is rewritten.
pub fn normalize_key(key: &str) -> String {
    key.replace('-', "_")
}

/// Recursively normalize every key in `table`, including nested tables and
/// tables nested inside arrays. Operates in place.
pub fn normalize_table(table: &mut Table) {
    let original_keys: Vec<String> = table.keys().cloned().collect();
    for key in original_keys {
        // Always remove + re-insert so we can recurse into the value even when
        // the key itself doesn't change (e.g. `pool_size`).
        if let Some(mut value) = table.remove(&key) {
            normalize_value(&mut value);
            let new_key = normalize_key(&key);
            table.insert(new_key, value);
        }
    }
}

fn normalize_value(value: &mut Value) {
    match value {
        Value::Table(t) => normalize_table(t),
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                normalize_value(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(toml_str: &str) -> Table {
        toml_str.parse::<Table>().unwrap()
    }

    #[test]
    fn normalize_key_replaces_dashes() {
        assert_eq!(normalize_key("pool-size"), "pool_size");
        assert_eq!(normalize_key("foo-bar-baz"), "foo_bar_baz");
    }

    #[test]
    fn normalize_key_preserves_existing_underscores() {
        assert_eq!(normalize_key("pool_size"), "pool_size");
        assert_eq!(normalize_key("mixed-name_field"), "mixed_name_field");
    }

    #[test]
    fn normalize_key_preserves_dots() {
        // Dotted paths used for CLI/URL overrides must keep their separators.
        assert_eq!(normalize_key("database.pool-size"), "database.pool_size");
    }

    #[test]
    fn normalize_key_empty() {
        assert_eq!(normalize_key(""), "");
    }

    #[test]
    fn normalize_key_no_dashes_is_noop() {
        assert_eq!(normalize_key("plain"), "plain");
    }

    // -- Table walking ---------------------------------------------------------

    #[test]
    fn normalize_table_top_level_keys() {
        let mut t = table(r#"pool-size = 10"#);
        normalize_table(&mut t);
        assert_eq!(t["pool_size"].as_integer().unwrap(), 10);
        assert!(!t.contains_key("pool-size"));
    }

    #[test]
    fn normalize_table_recurses_into_nested_tables() {
        let mut t = table(
            r#"
            [my-database]
            pool-size = 20
            "#,
        );
        normalize_table(&mut t);
        let db = t["my_database"].as_table().unwrap();
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
    }

    #[test]
    fn normalize_table_recurses_through_arrays_of_tables() {
        let mut t = table(
            r#"
            [[my-list]]
            kebab-key = 1

            [[my-list]]
            kebab-key = 2
            "#,
        );
        normalize_table(&mut t);
        let arr = t["my_list"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["kebab_key"].as_integer().unwrap(), 1);
        assert_eq!(arr[1]["kebab_key"].as_integer().unwrap(), 2);
    }

    #[test]
    fn normalize_table_leaves_scalar_values_untouched() {
        // Only keys are rewritten — string values containing `-` must survive.
        let mut t = table(r#"url = "pg://host-with-dash""#);
        normalize_table(&mut t);
        assert_eq!(t["url"].as_str().unwrap(), "pg://host-with-dash");
    }

    #[test]
    fn normalize_table_mixed_keys() {
        let mut t = table(
            r#"
            already_snake = 1
            kebab-key = 2
            mixed-name_thing = 3
            "#,
        );
        normalize_table(&mut t);
        assert_eq!(t["already_snake"].as_integer().unwrap(), 1);
        assert_eq!(t["kebab_key"].as_integer().unwrap(), 2);
        assert_eq!(t["mixed_name_thing"].as_integer().unwrap(), 3);
    }

    #[test]
    fn normalize_table_empty_is_noop() {
        let mut t = Table::new();
        normalize_table(&mut t);
        assert!(t.is_empty());
    }

    #[test]
    fn normalize_table_deeply_nested() {
        let mut t = table(
            r#"
            [a-1]
            [a-1.b-2]
            [a-1.b-2.c-3]
            leaf-key = "v"
            "#,
        );
        normalize_table(&mut t);
        let leaf = t["a_1"]["b_2"]["c_3"]["leaf_key"].as_str().unwrap();
        assert_eq!(leaf, "v");
    }
}
