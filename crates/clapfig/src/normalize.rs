//! Kebab-case → snake_case key normalization.
//!
//! When [`normalize_keys`](crate::Builder::normalize_keys)
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

use crate::format::{ConfigPath, PathSegment, SpanEntry};
use crate::runtime::{Schema, Shape};
use crate::value::{Map, Value};

/// Two distinct keys in the same table collapsed to the same normalized form.
/// Surfaced from [`check_collisions`] — the whole-document detection pass
/// shared by load ([`normalize_table`]), persistence (set/unset), and
/// scoped get — and wrapped via [`KeyCollision::into_error`] with the
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

impl KeyCollision {
    /// Wrap into [`ClapfigError::NormalizedKeyCollision`], stamping the
    /// owning file's path. Every surfacing site — load, persistence,
    /// scoped get — goes through this one constructor.
    pub(crate) fn into_error(self, path: &std::path::Path) -> crate::error::ClapfigError {
        crate::error::ClapfigError::NormalizedKeyCollision {
            path: path.to_path_buf(),
            section: self.section,
            normalized_key: self.normalized_key,
            originals: self.originals,
        }
    }
}

/// Replace every `-` with `_` in a single key string.
///
/// Used for dotted CLI/URL override paths (`"database.pool-size"`
/// → `"database.pool_size"`) — `.` segment separators are preserved because
/// only `-` is rewritten.
pub fn normalize_key(key: &str) -> String {
    key.replace('-', "_")
}

/// Replace every `_` with `-` in a single key string — the inverse of
/// [`normalize_key`], producing the spelling clapfig EMITS when
/// normalization is enabled (template keys, and keys the persistence path
/// writes for paths not already present in a document).
pub fn kebab_key(key: &str) -> String {
    key.replace('_', "-")
}

/// A declared schema key that input canonicalization can never produce.
///
/// [`normalize_key`] rewrites every `-` to `_`, so a key reaching
/// validation never holds a `-`. A field declared with one — from
/// `rename_all = "kebab-case"`, from `SCREAMING-KEBAB-CASE`, or from an
/// explicit rename — is therefore unreachable under
/// [`normalize_keys(true)`](crate::Builder::normalize_keys): whatever
/// spelling a user writes normalizes to something else, so the loader
/// answers with `UnknownKeys` for the key it did get and, if the field
/// is required, `MissingRequired` for the one it did not. Surfaced from
/// [`check_shape_reachable`] and wrapped via
/// [`UnreachableKey::into_error`].
#[derive(Debug, Clone)]
pub(crate) struct UnreachableKey {
    /// Dotted path to the section declaring the key. Empty for the
    /// document root.
    pub section: String,
    /// The declared name no written spelling normalizes to.
    pub key: String,
}

impl UnreachableKey {
    /// Wrap into [`ClapfigError::UnreachableNormalizedKey`], computing
    /// the name the key actually arrives under. Every surfacing site —
    /// template generation (`config gen` and the missing-file seeding
    /// `config set` does), JSON Schema generation, the artifact pair —
    /// goes through this one constructor.
    ///
    /// [`ClapfigError::UnreachableNormalizedKey`]: crate::error::ClapfigError::UnreachableNormalizedKey
    pub(crate) fn into_error(self) -> crate::error::ClapfigError {
        crate::error::ClapfigError::UnreachableNormalizedKey {
            normalized: normalize_key(&self.key),
            section: self.section,
            key: self.key,
        }
    }
}

/// Whole-shape reachability check for a `normalize_keys(true)` builder:
/// every key the shape declares, at every depth, must be stable under
/// [`normalize_key`] — otherwise nothing a user can write reaches it.
///
/// The generation-side counterpart to [`check_collisions`]. Load already
/// refuses a document whose keys miss such a field, key by key; this
/// refuses the artifacts — template, JSON Schema, or the pair — that
/// would otherwise be generated under names that same loader rejects.
/// Template generation is the seam `config set` seeds a missing scope
/// file through, so the refusal reaches persistence too.
/// Callers that do not normalize skip it: a declared `listen-port` is
/// matched literally and is perfectly reachable then.
pub(crate) fn check_shape_reachable(shape: &Shape) -> Result<(), UnreachableKey> {
    reachable_at(shape, "")
}

fn reachable_at(shape: &Shape, section: &str) -> Result<(), UnreachableKey> {
    match shape {
        Shape::Leaf(_) => Ok(()),
        Shape::Object(schema) => reachable_fields(schema, section),
        // An array element is addressed by index and a map entry by a
        // user-chosen key; normalization respells neither. Only the
        // item's own declared fields are schema keys.
        Shape::Array(array) => reachable_at(&array.item, section),
        Shape::Map(map) => reachable_at(&map.item, section),
        Shape::Tagged(tagged) => {
            // The tag is a key like any other. Discriminators are
            // *values* and are never normalized.
            reachable_key(&tagged.tag, section)?;
            for variant in &tagged.variants {
                reachable_fields(&variant.schema, section)?;
            }
            Ok(())
        }
    }
}

fn reachable_fields(schema: &Schema, section: &str) -> Result<(), UnreachableKey> {
    for field in &schema.fields {
        reachable_key(&field.name, section)?;
        let nested = if section.is_empty() {
            field.name.clone()
        } else {
            format!("{section}.{}", field.name)
        };
        reachable_at(&field.field, &nested)?;
    }
    Ok(())
}

fn reachable_key(key: &str, section: &str) -> Result<(), UnreachableKey> {
    if normalize_key(key) == key {
        Ok(())
    } else {
        Err(UnreachableKey {
            section: section.to_string(),
            key: key.to_string(),
        })
    }
}

/// Resolve one canonical snake_case path segment against a table's keys
/// under dash/underscore equivalence, returning the matching key's
/// concrete spelling (`None` when no key matches).
///
/// Callers have already validated the whole document with
/// [`check_collisions`], so at most one key can match — this function
/// only chooses the concrete spelling, it does not arbitrate between
/// equivalent keys.
pub(crate) fn resolve_table_key<'a>(table: &'a Map, segment: &str) -> Option<&'a String> {
    table.keys().find(|k| normalize_key(k) == segment)
}

/// Non-mutating whole-document collision check: every table at every
/// depth (including tables nested inside arrays) must hold at most one
/// spelling per normalized key. The single detection pass behind the
/// never-silently-compete rule: load ([`normalize_table`]) runs it before
/// rewriting, and normalized set/unset (`persist::resolve_document_path`)
/// and scoped get (`ops::table_get_normalized`) run it
/// before resolving the requested path — so an ambiguous document fails
/// those operations even when the collision sits at a key or table the
/// requested path never touches. A document the load path refuses is
/// never edited or queried.
pub(crate) fn check_collisions(table: &Map) -> Result<(), KeyCollision> {
    check_at(table, "")
}

fn check_at(table: &Map, section: &str) -> Result<(), KeyCollision> {
    // Bucket this level's keys by normalized form. BTreeMap so the
    // iteration that picks an offending bucket is deterministic, and
    // `originals` (fed in BTreeMap key order) comes out sorted.
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

    for (key, value) in table {
        let normalized = normalize_key(key);
        let nested_section = if section.is_empty() {
            normalized
        } else {
            format!("{section}.{normalized}")
        };
        check_value(value, &nested_section)?;
    }
    Ok(())
}

fn check_value(value: &Value, section: &str) -> Result<(), KeyCollision> {
    match value {
        Value::Map(t) => check_at(t, section),
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let nested = format!("{section}[{i}]");
                check_value(item, &nested)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Recursively normalize every key in `table`, including nested tables and
/// tables nested inside arrays. Operates in place.
///
/// Runs [`check_collisions`] over the whole tree before mutating
/// anything: if two distinct keys at any table level would normalize to
/// the same name, returns `Err(KeyCollision)` with the table's dotted
/// section path and the offending source keys, and the table is left in
/// its original state at every depth. On success, all dash-bearing keys
/// have been rewritten with `-` → `_`.
pub fn normalize_table(table: &mut Map) -> Result<(), KeyCollision> {
    check_collisions(table)?;
    rewrite_table(table);
    Ok(())
}

/// Normalize a parsed table and rewrite span-index paths with the same
/// `-` → `_` rule (ADR-0006). Collision check runs once before either
/// rewrite; span entries themselves are unchanged so the snippet still
/// shows the user's original spelling.
pub(crate) fn normalize_table_and_spans(
    table: &mut Map,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Result<(), KeyCollision> {
    normalize_table(table)?;
    normalize_spans(spans);
    Ok(())
}

/// Rewrite span-index path keys with the same `-` → `_` rule as
/// [`normalize_table`]. Index segments are unchanged.
pub(crate) fn normalize_spans(spans: &mut BTreeMap<ConfigPath, SpanEntry>) {
    let old = std::mem::take(spans);
    for (path, entry) in old {
        spans.insert(normalize_config_path(path), entry);
    }
}

fn normalize_config_path(path: ConfigPath) -> ConfigPath {
    let mut out = ConfigPath::new();
    for segment in path.segments() {
        out = match segment {
            PathSegment::Key(k) => out.key(normalize_key(k)),
            PathSegment::Index(i) => out.index(*i),
        };
    }
    out
}

fn rewrite_table(table: &mut Map) {
    // `mem::take` lets us iterate the table by-value (no key cloning, no
    // transient remove+insert per entry); the empty table we leave behind
    // is then refilled with normalized keys.
    let old = std::mem::take(table);
    for (key, mut value) in old {
        rewrite_value(&mut value);
        table.insert(normalize_key(&key), value);
    }
}

fn rewrite_value(value: &mut Value) {
    match value {
        Value::Map(t) => rewrite_table(t),
        Value::Array(arr) => arr.iter_mut().for_each(rewrite_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(toml_str: &str) -> Map {
        crate::fixtures::test::parse_toml(toml_str)
    }

    // -- Reachability under normalization ---------------------------------
    //
    // `check_shape_reachable` answers one question per declared key: can
    // anything a user writes normalize to it? The walk has to reach every
    // key position a document can address — object fields at any depth,
    // an array element's fields, a map entry's fields, and a tagged
    // union's tag and per-variant fields — because a schema is refused or
    // accepted as a whole.

    fn kebab_field_object() -> crate::runtime::Schema {
        crate::runtime::Schema::object("Inner")
            .field("listen-port", crate::runtime::Field::integer())
            .build()
    }

    fn unreachable(shape: &Shape) -> UnreachableKey {
        check_shape_reachable(shape).expect_err("kebab-declared key must be refused")
    }

    #[test]
    fn a_shape_declaring_only_normalized_keys_is_reachable() {
        // Every name here survives `-` → `_` unchanged, so each has a
        // written spelling that reaches it. camelCase included: it holds
        // no `-`, so normalization leaves it alone.
        let shape = Shape::from(
            crate::runtime::Schema::object("App")
                .field("host", crate::runtime::Field::string())
                .field("pool_size", crate::runtime::Field::integer())
                .field("maxRetries", crate::runtime::Field::integer())
                .build(),
        );
        assert!(check_shape_reachable(&shape).is_ok());
    }

    #[test]
    fn a_root_field_holding_a_dash_is_unreachable() {
        let shape = Shape::from(
            crate::runtime::Schema::object("App")
                .field("listen-port", crate::runtime::Field::integer())
                .build(),
        );
        let err = unreachable(&shape);
        assert_eq!(err.key, "listen-port");
        assert_eq!(err.section, "", "a root key has no section");
        assert!(
            err.into_error().to_string().contains("listen_port"),
            "the message names the spelling the loader actually looks for"
        );
    }

    #[test]
    fn the_walk_descends_into_nested_objects_arrays_and_maps() {
        // Same offending key, reached three different ways — and each
        // reports the dotted path to the section that declares it.
        for (label, field) in [
            ("nested", Shape::Object(kebab_field_object())),
            (
                "array",
                Shape::from(crate::runtime::Field::array_of_type(kebab_field_object())),
            ),
            (
                "map",
                Shape::from(crate::runtime::Field::map_of(kebab_field_object())),
            ),
        ] {
            let shape = Shape::from(
                crate::runtime::Schema::object("App")
                    .field("section", field)
                    .build(),
            );
            let err = unreachable(&shape);
            assert_eq!(err.key, "listen-port", "{label}");
            assert_eq!(err.section, "section", "{label}");
        }
    }

    #[test]
    fn a_tagged_union_checks_its_tag_and_every_variant_field() {
        // The tag is a key, so a multiword kebab tag is unreachable...
        let tagged_tag = Shape::tagged("Block", "block-kind")
            .variant(
                "rust",
                crate::runtime::Schema::object("Rust")
                    .field("mount", crate::runtime::Field::string())
                    .build(),
            )
            .build();
        let err = unreachable(&Shape::Tagged(tagged_tag));
        assert_eq!(err.key, "block-kind");

        // ...and so is a kebab field inside any one variant, even when
        // the tag and the other variants are clean.
        let tagged_field = Shape::tagged("Block", "kind")
            .variant(
                "rust",
                crate::runtime::Schema::object("Rust")
                    .field("mount", crate::runtime::Field::string())
                    .build(),
            )
            .variant("payload", kebab_field_object())
            .build();
        let err = unreachable(&Shape::Tagged(tagged_field));
        assert_eq!(err.key, "listen-port");
    }

    #[test]
    fn map_and_array_positions_are_not_themselves_keys() {
        // A map's entry keys are user data and an array's positions are
        // indices; neither is a declared name, so a map of plain strings
        // is reachable however its entries end up spelled.
        let shape = Shape::from(
            crate::runtime::Schema::object("App")
                .field(
                    "labels",
                    crate::runtime::Field::map_of(crate::runtime::Field::string()),
                )
                .build(),
        );
        assert!(check_shape_reachable(&shape).is_ok());
    }

    #[test]
    fn resolve_table_key_picks_the_concrete_spelling() {
        let t = table("pool-size = 1\nother = 2\n");
        // Single equivalent spelling: resolves to the concrete key.
        assert_eq!(resolve_table_key(&t, "pool_size").unwrap(), "pool-size");
        // Exact spelling resolves to itself.
        assert_eq!(resolve_table_key(&t, "other").unwrap(), "other");
        // No match.
        assert!(resolve_table_key(&t, "missing").is_none());
    }

    #[test]
    fn check_collisions_accepts_a_clean_tree() {
        let t = table(
            r#"
            pool-size = 1
            other_key = 2

            [nested-section]
            leaf-key = 3
            "#,
        );
        check_collisions(&t).unwrap();
    }

    #[test]
    fn check_collisions_finds_collision_in_any_table() {
        // The check is whole-document: a collision inside a nested table
        // is found without any requested path steering traversal there,
        // reported with the normalized section path.
        let t = table(
            r#"
            host = "h"

            [data-base]
            pool-size = 5
            pool_size = 6
            "#,
        );
        let err = check_collisions(&t).unwrap_err();
        assert_eq!(err.section, "data_base");
        assert_eq!(err.normalized_key, "pool_size");
        assert_eq!(err.originals, vec!["pool-size", "pool_size"]);
    }

    #[test]
    fn check_collisions_reaches_tables_inside_arrays() {
        let t = table(
            r#"
            [[items]]
            fine = 1

            [[items]]
            kebab-key = 1
            kebab_key = 2
            "#,
        );
        let err = check_collisions(&t).unwrap_err();
        assert_eq!(err.section, "items[1]");
        assert_eq!(err.normalized_key, "kebab_key");
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

    #[test]
    fn kebab_key_is_the_inverse_spelling() {
        assert_eq!(kebab_key("pool_size"), "pool-size");
        assert_eq!(kebab_key("plain"), "plain");
        assert_eq!(kebab_key("already-kebab"), "already-kebab");
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
        let db = t["my_database"].as_map().unwrap();
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
        let mut t = Map::new();
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
        // Regression: collision detection is a whole-tree pre-flight, so
        // callers that catch the error can rely on the table still being
        // in its original state (no half-normalized aftermath to clean up).
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

        // Same guarantee when the collision is nested: keys at shallower
        // depths are untouched too.
        let mut t = table(
            r#"
            unrelated-ok = 1

            [my-section]
            kebab-key = 1
            kebab_key = 2
            "#,
        );
        assert!(normalize_table(&mut t).is_err());
        assert!(t.contains_key("unrelated-ok"));
        assert!(t.contains_key("my-section"));
    }

    #[test]
    fn normalize_spans_rewrites_key_segments_and_keeps_indexes() {
        use crate::format::Span;

        let mut spans = BTreeMap::new();
        let kebab = ConfigPath::new().key("my-list").index(1).key("kebab-key");
        let entry = SpanEntry {
            key: Some(Span { start: 10, end: 19 }),
            value: Span { start: 22, end: 23 },
        };
        spans.insert(kebab, entry);
        normalize_spans(&mut spans);
        let snake = ConfigPath::new().key("my_list").index(1).key("kebab_key");
        assert_eq!(spans.get(&snake).copied(), Some(entry));
        assert_eq!(spans.len(), 1);
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
