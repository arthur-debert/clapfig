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
//!
//! If two distinct keys in the same table normalize to the same name (e.g.
//! `pool-size` and `pool_size` both → `pool_size`), [`normalize_table`]
//! returns a [`KeyCollision`] error rather than silently dropping one entry
//! — the resolution would otherwise depend on the table's internal key
//! iteration order.

use std::collections::BTreeMap;

use toml::{Table, Value};

/// Two distinct keys in the same table collapsed to the same normalized form.
/// Surfaced from [`normalize_table`] and wrapped at the call site with the
/// owning file's path.
#[derive(Debug, Clone)]
pub struct KeyCollision {
    /// Dotted path to the table that contains the collision. Empty for the
    /// top-level table.
    pub section: String,
    /// The normalized key that two or more source keys produced.
    pub normalized_key: String,
    /// The original keys (sorted) that collapsed to `normalized_key`.
    pub originals: Vec<String>,
}

/// Replace every `-` with `_` in a single key string.
///
/// Used for dotted CLI/URL override paths (`"database.pool-size"`
/// → `"database.pool_size"`) — `.` segment separators are preserved because
/// only `-` is rewritten. Skips the allocation entirely when the key has no
/// `-` characters (the common case once a struct's snake_case fields are
/// being used directly).
pub fn normalize_key(key: &str) -> String {
    if key.contains('-') {
        key.replace('-', "_")
    } else {
        key.to_owned()
    }
}

/// Recursively normalize every key in `table`, including nested tables and
/// tables nested inside arrays. Operates in place.
///
/// Detects collisions before mutating: if two distinct keys at the same
/// table level would normalize to the same name, returns
/// `Err(KeyCollision)` with the table's dotted section path and the
/// offending source keys. On success, all dash-bearing keys have been
/// rewritten with `-` → `_`.
pub fn normalize_table(table: &mut Table) -> Result<(), KeyCollision> {
    normalize_at(table, "")
}

fn normalize_at(table: &mut Table, section: &str) -> Result<(), KeyCollision> {
    // First pass: detect collisions before mutating anything. BTreeMap so
    // the iteration that picks an offending bucket is deterministic.
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for k in table.keys() {
        buckets.entry(normalize_key(k)).or_default().push(k.clone());
    }
    for (normalized_key, mut originals) in buckets {
        if originals.len() > 1 {
            originals.sort();
            return Err(KeyCollision {
                section: section.to_string(),
                normalized_key,
                originals,
            });
        }
    }

    // Second pass: rewrite in place. `mem::take` lets us iterate the table
    // by-value (no key cloning, no transient remove+insert per entry); the
    // empty table we leave behind is then refilled with normalized keys.
    let old = std::mem::take(table);
    for (key, mut value) in old {
        let new_key = normalize_key(&key);
        let nested_section = if section.is_empty() {
            new_key.clone()
        } else {
            format!("{section}.{new_key}")
        };
        normalize_value(&mut value, &nested_section)?;
        table.insert(new_key, value);
    }
    Ok(())
}

fn normalize_value(value: &mut Value, section: &str) -> Result<(), KeyCollision> {
    match value {
        Value::Table(t) => normalize_at(t, section),
        Value::Array(arr) => {
            for (i, item) in arr.iter_mut().enumerate() {
                let nested = format!("{section}[{i}]");
                normalize_value(item, &nested)?;
            }
            Ok(())
        }
        _ => Ok(()),
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
        normalize_table(&mut t).unwrap();
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
        normalize_table(&mut t).unwrap();
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
        normalize_table(&mut t).unwrap();
        let arr = t["my_list"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["kebab_key"].as_integer().unwrap(), 1);
        assert_eq!(arr[1]["kebab_key"].as_integer().unwrap(), 2);
    }

    #[test]
    fn normalize_table_leaves_scalar_values_untouched() {
        // Only keys are rewritten — string values containing `-` must survive.
        let mut t = table(r#"url = "pg://host-with-dash""#);
        normalize_table(&mut t).unwrap();
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
        normalize_table(&mut t).unwrap();
        assert_eq!(t["already_snake"].as_integer().unwrap(), 1);
        assert_eq!(t["kebab_key"].as_integer().unwrap(), 2);
        assert_eq!(t["mixed_name_thing"].as_integer().unwrap(), 3);
    }

    #[test]
    fn normalize_table_empty_is_noop() {
        let mut t = Table::new();
        normalize_table(&mut t).unwrap();
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
        normalize_table(&mut t).unwrap();
        let leaf = t["a_1"]["b_2"]["c_3"]["leaf_key"].as_str().unwrap();
        assert_eq!(leaf, "v");
    }

    // -- Collision detection --------------------------------------------------

    #[test]
    fn normalize_table_errors_on_top_level_collision() {
        let mut t = table(
            r#"
            pool-size = 5
            pool_size = 10
            "#,
        );
        let err = normalize_table(&mut t).unwrap_err();
        assert_eq!(err.section, "");
        assert_eq!(err.normalized_key, "pool_size");
        assert_eq!(err.originals, vec!["pool-size", "pool_size"]);
    }

    #[test]
    fn normalize_table_errors_on_nested_collision_with_section_path() {
        let mut t = table(
            r#"
            [database]
            pool-size = 5
            pool_size = 10
            "#,
        );
        let err = normalize_table(&mut t).unwrap_err();
        assert_eq!(err.section, "database");
        assert_eq!(err.normalized_key, "pool_size");
        assert_eq!(err.originals, vec!["pool-size", "pool_size"]);
    }

    #[test]
    fn normalize_table_collision_inside_array_of_tables() {
        let mut t = table(
            r#"
            [[items]]
            kebab-key = 1
            kebab_key = 2
            "#,
        );
        let err = normalize_table(&mut t).unwrap_err();
        // The section path includes the array index for the offending entry.
        assert_eq!(err.section, "items[0]");
        assert_eq!(err.normalized_key, "kebab_key");
    }

    #[test]
    fn normalize_table_collision_does_not_partially_mutate() {
        // Regression: collision detection is pre-flight, so callers that
        // catch the error can rely on the table still being in its original
        // state (no half-normalized aftermath to clean up).
        let mut t = table(
            r#"
            unrelated-ok = 1
            pool-size = 5
            pool_size = 10
            "#,
        );
        assert!(normalize_table(&mut t).is_err());
        // The dash-bearing sibling key should still be in kebab form.
        assert!(t.contains_key("unrelated-ok"));
    }

    #[test]
    fn normalize_table_no_false_collision_when_only_snake() {
        // Two ordinary snake keys that don't share a normalized form should
        // pass through cleanly even with normalization on.
        let mut t = table(
            r#"
            pool_size = 5
            other_key = 10
            "#,
        );
        normalize_table(&mut t).unwrap();
        assert_eq!(t["pool_size"].as_integer().unwrap(), 5);
        assert_eq!(t["other_key"].as_integer().unwrap(), 10);
    }
}
