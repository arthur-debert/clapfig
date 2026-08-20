//! Convert dotted-key CLI overrides into a nested config value [`Map`].
//!
//! Each `("database.url", Value)` pair is expanded into the nested map structure
//! needed for deep-merge with other config layers.

use std::collections::HashSet;

use crate::origin::{Origin, OriginMap, OriginNode};
use crate::runtime::Schema;
use crate::value::{Map, Value};

/// Convert dotted-key overrides into a nested config value [`Map`].
///
/// `("database.url", Value::String("pg://"))` becomes `{database = {url = "pg://"}}`
///
/// If multiple entries target the same key, the last one wins.
#[cfg(test)]
pub fn overrides_to_table(entries: &[(String, Value)]) -> Map {
    let triples: Vec<(String, String, Value)> = entries
        .iter()
        .map(|(k, v)| (k.clone(), k.clone(), v.clone()))
        .collect();
    overrides_to_table_with_original_keys(&triples, |key| Origin::r#override(key)).0
}

/// Convert dotted-key overrides into a nested value map **and** a
/// lockstep origin map. Each entry is `(merge_key, original_key, value)`:
/// `merge_key` is the path after `normalize_keys`, `original_key` is the
/// spelling the origin stores (URL query keys stay percent-decoded and
/// dotted; override keys stay the caller-supplied spelling).
pub(crate) fn overrides_to_table_with_original_keys(
    entries: &[(String, String, Value)],
    origin_for: impl Fn(&str) -> Origin,
) -> (Map, OriginMap) {
    let mut table = Map::new();
    let mut origins = OriginMap::new();
    for (merge_key, original_key, value) in entries {
        set_nested_with_origin(
            &mut table,
            &mut origins,
            merge_key,
            value.clone(),
            origin_for(original_key),
        );
    }
    (table, origins)
}

fn set_nested_with_origin(
    table: &mut Map,
    origins: &mut OriginMap,
    dotted_key: &str,
    value: Value,
    origin: Origin,
) {
    let segments: Vec<&str> = dotted_key.split('.').collect();
    let (leaf, parents) = segments
        .split_last()
        .expect("split('.') always yields at least one segment");
    let mut current = table;
    let mut current_origins = origins;
    for segment in parents {
        current = current
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Map(Map::new()))
            .as_map_mut()
            .expect("clapfig: override path conflict — intermediate key is not a map");
        let node = current_origins
            .entry((*segment).to_string())
            .or_insert_with(|| OriginNode::map(origin.clone(), OriginMap::new()));
        current_origins = node.map_children_mut();
    }
    current.insert((*leaf).to_string(), value.clone());
    current_origins.insert((*leaf).to_string(), OriginNode::from_value(&value, origin));
}

/// Collect all valid leaf key paths from a schema.
///
/// Returns dotted paths like `"host"`, `"database.url"`, `"database.pool_size"`.
/// Section names (nested structs) are excluded — only leaf fields are returned.
pub fn valid_keys(schema: &Schema) -> HashSet<String> {
    let mut keys = HashSet::new();
    collect_keys(schema, "", &mut keys);
    keys
}

/// Addressable dotted keys for a document-root [`Shape`].
///
/// Object roots use [`valid_keys`]. A root Map has no addressable keys
/// (every segment is user data).
pub fn valid_keys_shape(shape: &crate::runtime::Shape) -> HashSet<String> {
    match shape {
        crate::runtime::Shape::Object(schema) => valid_keys(schema),
        crate::runtime::Shape::Map(_)
        | crate::runtime::Shape::Tagged(_)
        | crate::runtime::Shape::Leaf(_)
        | crate::runtime::Shape::Array(_) => HashSet::new(),
    }
}

fn collect_keys(schema: &Schema, prefix: &str, keys: &mut HashSet<String>) {
    for field in &schema.fields {
        let dotted = if prefix.is_empty() {
            field.name.to_string()
        } else {
            format!("{prefix}.{}", field.name)
        };
        match &field.field {
            crate::runtime::Shape::Leaf(_) => {
                keys.insert(dotted);
            }
            crate::runtime::Shape::Object(nested) => {
                collect_keys(nested, &dotted, keys);
            }
            crate::runtime::Shape::Array(array) if array.item.is_value_field() => {
                // Homogeneous array of leaves is addressable as one key
                // (`tags = ["a", "b"]`), like the old LeafType::Array.
                keys.insert(dotted);
            }
            crate::runtime::Shape::Map(map) if map.item.is_value_field() => {
                keys.insert(dotted);
            }
            crate::runtime::Shape::Array(_) => {
                // Skip array-of-objects subtrees. Dotted-key consumers
                // (cli_overrides, url_query, persist set/unset) build
                // nested tables, not arrays-of-tables, so listing
                // `plugins.name` as valid would let `config set
                // plugins.name foo` write `[plugins] name = "foo"` —
                // which then fails runtime validation with "expected
                // array, got map". The right surface for setting
                // entries inside an array of tables would be an indexed
                // dotted syntax (`plugins[0].name`) that none of the
                // current consumers parses. Until then, those keys are
                // not addressable by dotted path.
            }
            crate::runtime::Shape::Map(_) => {
                // Skip map-of-objects subtrees for the same reason:
                // dotted-key consumers can't tell whether
                // `plugins.audit` means "set the `audit` entry" (a real
                // user-supplied key) or "set the `audit` leaf inside a
                // known schema field."
            }
            crate::runtime::Shape::Tagged(_) => {}
        }
    }
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
        let db = table["database"].as_map().unwrap();
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
        let db = table["database"].as_map().unwrap();
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

    // --- valid_keys tests ---

    #[test]
    fn valid_keys_collects_all_leaf_paths() {
        let schema = crate::fixtures::test::test_schema();
        let keys = valid_keys(&schema);
        assert!(keys.contains("host"));
        assert!(keys.contains("port"));
        assert!(keys.contains("debug"));
        assert!(keys.contains("database.url"));
        assert!(keys.contains("database.pool_size"));
        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn valid_keys_excludes_section_names() {
        let schema = crate::fixtures::test::test_schema();
        let keys = valid_keys(&schema);
        assert!(!keys.contains("database"));
    }
}
