//! Recursive deep-merge for config value [`Map`]s.
//!
//! Used to combine sparse config layers — each layer only specifies the keys it
//! overrides, and nested maps are merged recursively rather than replaced wholesale.

use crate::value::{Map, Value};

/// Deep-merge `overlay` on top of `base`.
/// If both sides have a Map for the same key, recurse.
/// Otherwise, `overlay`'s value wins.
pub fn deep_merge(mut base: Map, overlay: Map) -> Map {
    for (key, overlay_val) in overlay {
        match (base.remove(&key), overlay_val) {
            (Some(Value::Map(base_map)), Value::Map(overlay_map)) => {
                base.insert(key, Value::Map(deep_merge(base_map, overlay_map)));
            }
            (_, overlay_val) => {
                base.insert(key, overlay_val);
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(toml_str: &str) -> Map {
        crate::fixtures::test::parse_toml(toml_str)
    }

    #[test]
    fn disjoint_keys_merge() {
        let base = table(r#"host = "localhost""#);
        let overlay = table("port = 3000");
        let merged = deep_merge(base, overlay);
        assert_eq!(merged["host"].as_str().unwrap(), "localhost");
        assert_eq!(merged["port"].as_integer().unwrap(), 3000);
    }

    #[test]
    fn same_scalar_key_overlay_wins() {
        let base = table("port = 8080");
        let overlay = table("port = 3000");
        let merged = deep_merge(base, overlay);
        assert_eq!(merged["port"].as_integer().unwrap(), 3000);
    }

    #[test]
    fn nested_tables_recurse() {
        let base = table(
            r#"
            [database]
            url = "postgres://old"
            pool_size = 5
            "#,
        );
        let overlay = table(
            r#"
            [database]
            pool_size = 20
            "#,
        );
        let merged = deep_merge(base, overlay);
        let db = merged["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "postgres://old");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
    }

    #[test]
    fn overlay_scalar_replaces_table() {
        let base = table(
            r#"
            [database]
            url = "x"
            "#,
        );
        let overlay = table(r#"database = "flat_string""#);
        let merged = deep_merge(base, overlay);
        assert_eq!(merged["database"].as_str().unwrap(), "flat_string");
    }

    #[test]
    fn empty_overlay_returns_base() {
        let base = table("port = 8080");
        let merged = deep_merge(base.clone(), Map::new());
        assert_eq!(merged, base);
    }

    #[test]
    fn empty_base_returns_overlay() {
        let overlay = table("port = 3000");
        let merged = deep_merge(Map::new(), overlay.clone());
        assert_eq!(merged, overlay);
    }

    #[test]
    fn deeply_nested_three_levels() {
        let base = table(
            r#"
            [a]
            [a.b]
            [a.b.c]
            val = 1
            other = "keep"
            "#,
        );
        let overlay = table(
            r#"
            [a]
            [a.b]
            [a.b.c]
            val = 99
            "#,
        );
        let merged = deep_merge(base, overlay);
        let c = merged["a"]["b"]["c"].as_map().unwrap();
        assert_eq!(c["val"].as_integer().unwrap(), 99);
        assert_eq!(c["other"].as_str().unwrap(), "keep");
    }

    #[test]
    fn multiple_sequential_merges() {
        let a = table(r#"host = "a""#);
        let b = table("port = 1000");
        let c = table(r#"host = "c""#);
        let merged = deep_merge(deep_merge(a, b), c);
        assert_eq!(merged["host"].as_str().unwrap(), "c");
        assert_eq!(merged["port"].as_integer().unwrap(), 1000);
    }
}
