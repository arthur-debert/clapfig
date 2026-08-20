//! Recursive deep-merge for config value [`Map`]s.
//!
//! Used to combine sparse config layers — each layer only specifies the keys it
//! overrides, and nested maps are merged recursively rather than replaced wholesale.
//! Origins merge in the same walk (ADR-0004): when a value wins, its origin
//! wins. Arrays replace wholesale; the origin subtree replaces wholesale
//! with them.

use crate::origin::{Origin, OriginMap, OriginNode, take_map_children};
use crate::value::{Map, Value};

/// Deep-merge `overlay` on top of `base`, with origin trees in lockstep.
///
/// If both sides have a Map for the same key, recurse. Otherwise
/// `overlay`'s value **and** origin win. Arrays are not merged element-wise.
pub(crate) fn deep_merge(
    mut base: Map,
    overlay: Map,
    mut base_origins: OriginMap,
    mut overlay_origins: OriginMap,
) -> (Map, OriginMap) {
    for (key, overlay_val) in overlay {
        match (base.remove(&key), overlay_val) {
            (Some(Value::Map(base_map)), Value::Map(overlay_map)) => {
                let (base_origin, base_om) = take_map_children(base_origins.remove(&key));
                let (overlay_origin, overlay_om) = take_map_children(overlay_origins.remove(&key));
                let (merged, merged_o) = deep_merge(base_map, overlay_map, base_om, overlay_om);
                base.insert(key.clone(), Value::Map(merged));
                let origin = overlay_origin
                    .or(base_origin)
                    .unwrap_or_else(|| Origin::default(key.clone()));
                base_origins.insert(key, OriginNode::map(origin, merged_o));
            }
            (_, overlay_val) => {
                base.insert(key.clone(), overlay_val);
                if let Some(origin) = overlay_origins.remove(&key) {
                    base_origins.insert(key, origin);
                } else {
                    base_origins.remove(&key);
                }
            }
        }
    }
    (base, base_origins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ConfigPath, Span};
    use crate::origin::{OriginNode, lookup};
    use std::sync::Arc;

    fn table(toml_str: &str) -> Map {
        crate::fixtures::test::parse_toml(toml_str)
    }

    fn merge(base: Map, overlay: Map) -> Map {
        deep_merge(base, overlay, OriginMap::new(), OriginMap::new()).0
    }

    fn file_leaf(name: &str) -> OriginNode {
        OriginNode::leaf(crate::origin::Origin::file(
            name.into(),
            Span { start: 0, end: 1 },
            Arc::from(""),
        ))
    }

    fn file_map(name: &str, entries: OriginMap) -> OriginNode {
        OriginNode::map(
            crate::origin::Origin::file(name.into(), Span { start: 0, end: 1 }, Arc::from("")),
            entries,
        )
    }

    fn winner_file(origins: &OriginMap, dotted: &str) -> String {
        let mut path = ConfigPath::new();
        for seg in dotted.split('.') {
            path = path.key(seg);
        }
        lookup(origins, &path)
            .and_then(|o| o.file.as_ref())
            .map(|p| p.display().to_string())
            .expect("origin")
    }

    #[test]
    fn disjoint_keys_merge() {
        let base = table(r#"host = "localhost""#);
        let overlay = table("port = 3000");
        let merged = merge(base, overlay);
        assert_eq!(merged["host"].as_str().unwrap(), "localhost");
        assert_eq!(merged["port"].as_integer().unwrap(), 3000);
    }

    #[test]
    fn same_scalar_key_overlay_wins() {
        let base = table("port = 8080");
        let overlay = table("port = 3000");
        let merged = merge(base, overlay);
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
        let merged = merge(base, overlay);
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
        let merged = merge(base, overlay);
        assert_eq!(merged["database"].as_str().unwrap(), "flat_string");
    }

    #[test]
    fn empty_overlay_returns_base() {
        let base = table("port = 8080");
        let merged = merge(base.clone(), Map::new());
        assert_eq!(merged, base);
    }

    #[test]
    fn empty_base_returns_overlay() {
        let overlay = table("port = 3000");
        let merged = merge(Map::new(), overlay.clone());
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
        let merged = merge(base, overlay);
        let c = merged["a"]["b"]["c"].as_map().unwrap();
        assert_eq!(c["val"].as_integer().unwrap(), 99);
        assert_eq!(c["other"].as_str().unwrap(), "keep");
    }

    #[test]
    fn multiple_sequential_merges() {
        let a = table(r#"host = "a""#);
        let b = table("port = 1000");
        let c = table(r#"host = "c""#);
        let merged = merge(merge(a, b), c);
        assert_eq!(merged["host"].as_str().unwrap(), "c");
        assert_eq!(merged["port"].as_integer().unwrap(), 1000);
    }

    #[test]
    fn lockstep_nested_maps_keep_surviving_value_origins() {
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
        let mut base_o = OriginMap::new();
        let mut base_db = OriginMap::new();
        base_db.insert("url".into(), file_leaf("base.toml"));
        base_db.insert("pool_size".into(), file_leaf("base.toml"));
        base_o.insert("database".into(), file_map("base.toml", base_db));
        let mut overlay_o = OriginMap::new();
        let mut overlay_db = OriginMap::new();
        overlay_db.insert("pool_size".into(), file_leaf("local.toml"));
        overlay_o.insert("database".into(), file_map("local.toml", overlay_db));

        let (merged, origins) = deep_merge(base, overlay, base_o, overlay_o);
        let db = merged["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "postgres://old");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
        assert_eq!(winner_file(&origins, "database.url"), "base.toml");
        assert_eq!(winner_file(&origins, "database.pool_size"), "local.toml");
    }

    #[test]
    fn lockstep_array_replaces_wholesale_with_origin_subtree() {
        let mut base = Map::new();
        base.insert("tags".into(), Value::Array(vec![Value::String("a".into())]));
        let mut overlay = Map::new();
        overlay.insert(
            "tags".into(),
            Value::Array(vec![Value::String("b".into()), Value::String("c".into())]),
        );
        let mut base_o = OriginMap::new();
        base_o.insert(
            "tags".into(),
            OriginNode::array(
                crate::origin::Origin::file(
                    "base.toml".into(),
                    Span { start: 0, end: 1 },
                    Arc::from(""),
                ),
                vec![file_leaf("base.toml")],
            ),
        );
        let mut overlay_o = OriginMap::new();
        overlay_o.insert(
            "tags".into(),
            OriginNode::array(
                crate::origin::Origin::file(
                    "local.toml".into(),
                    Span { start: 0, end: 1 },
                    Arc::from(""),
                ),
                vec![file_leaf("local.toml"), file_leaf("local.toml")],
            ),
        );
        let (merged, origins) = deep_merge(base, overlay, base_o, overlay_o);
        assert_eq!(
            merged["tags"].as_array().unwrap().len(),
            2,
            "array replaced wholesale"
        );
        let tags = lookup(&origins, &ConfigPath::new().key("tags")).expect("tags origin");
        assert_eq!(
            tags.file.as_deref(),
            Some(std::path::Path::new("local.toml"))
        );
        let first = lookup(&origins, &ConfigPath::new().key("tags").index(0)).expect("item");
        assert_eq!(
            first.file.as_deref(),
            Some(std::path::Path::new("local.toml"))
        );
        assert!(
            lookup(&origins, &ConfigPath::new().key("tags").index(2)).is_none(),
            "base array length must not survive"
        );
    }

    #[test]
    fn lockstep_quoted_dotted_key_is_not_nested_path() {
        let mut base = Map::new();
        base.insert("a.b".into(), Value::Integer(1));
        let mut overlay = Map::new();
        overlay.insert("a.b".into(), Value::Integer(2));
        let mut base_o = OriginMap::new();
        base_o.insert("a.b".into(), file_leaf("base.toml"));
        let mut overlay_o = OriginMap::new();
        overlay_o.insert("a.b".into(), file_leaf("local.toml"));

        let (merged, origins) = deep_merge(base, overlay, base_o, overlay_o);
        assert_eq!(merged["a.b"].as_integer().unwrap(), 2);
        assert_eq!(
            lookup(&origins, &ConfigPath::new().key("a.b"))
                .and_then(|o| o.file.as_ref())
                .map(|p| p.display().to_string())
                .as_deref(),
            Some("local.toml")
        );
        assert!(
            lookup(&origins, &ConfigPath::new().key("a").key("b")).is_none(),
            "quoted dotted key is one segment, not [a] b"
        );
    }

    #[test]
    fn lockstep_overlay_without_origin_drops_base_origin() {
        let base = table("port = 8080");
        let overlay = table("port = 3000");
        let mut base_o = OriginMap::new();
        base_o.insert("port".into(), file_leaf("base.toml"));
        let (merged, origins) = deep_merge(base, overlay, base_o, OriginMap::new());
        assert_eq!(merged["port"].as_integer().unwrap(), 3000);
        assert!(
            lookup(&origins, &ConfigPath::new().key("port")).is_none(),
            "missing overlay origin must not leave the replaced base origin"
        );
    }

    #[test]
    fn lockstep_map_merge_keeps_base_origin_when_overlay_origin_missing() {
        let base = table(
            r#"
            [database]
            url = "postgres://old"
            "#,
        );
        let overlay = table(
            r#"
            [database]
            pool_size = 20
            "#,
        );
        let mut base_o = OriginMap::new();
        let mut base_db = OriginMap::new();
        base_db.insert("url".into(), file_leaf("base.toml"));
        base_o.insert("database".into(), file_map("base.toml", base_db));

        let (merged, origins) = deep_merge(base, overlay, base_o, OriginMap::new());
        let db = merged["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "postgres://old");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
        assert_eq!(winner_file(&origins, "database"), "base.toml");
        assert_eq!(winner_file(&origins, "database.url"), "base.toml");
        assert!(
            lookup(
                &origins,
                &ConfigPath::new().key("database").key("pool_size")
            )
            .is_none(),
            "overlay leaf with no origin must not inherit a synthesized default"
        );
    }
}
