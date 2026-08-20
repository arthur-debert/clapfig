//! Convert environment variables into a config value [`Map`] for merging
//! into config.
//!
//! Env vars matching `{PREFIX}__*` are collected, with `__` as the nesting separator
//! and segments lowercased to match Rust field names. Values are parsed heuristically
//! (bool > integer > float > string). Takes an iterator for testability.
//!
//! Each inserted path also records the original environment variable
//! name that produced it. Case-sensitive platforms accept
//! `MYAPP__rogue_key` as well as `MYAPP__ROGUE_KEY`; both collapse to
//! the same table path, but unknown-key errors must name the variable
//! the user actually has to unset — reconstructing an uppercased
//! spelling from the path would point at a different name. When several
//! source names collapse onto one path, the table last-wins (matching
//! insert order) and every source name is retained so the error can
//! list them all.
//!
//! Paths are [`ConfigPath`] identities, not flattened dotted strings:
//! `APP__PLUGINS__A.CONFIG__X` (`plugins."a.config".x`) and
//! `APP__PLUGINS__A__CONFIG__X` (`plugins.a.config.x`) coexist.
//! Provenance is a separate last-writer map ([`EnvWinners`]): when a
//! value wins, only that variable is the origin. Unknown-key aggregation
//! on [`EnvSources`] must not leak losing writers into `InvalidValue`.

use std::collections::BTreeMap;

use crate::format::ConfigPath;
use crate::value::{Map, Value};

/// Structured table path → the original environment variable name(s)
/// that produced it, in first-seen order. Several names can collapse
/// onto one path (`MYAPP__HOST` and `MYAPP__host` both become `host`);
/// the table last-wins, and this map keeps every spelling so unknown-key
/// reporting can name each variable to unset.
pub(crate) type EnvSources = BTreeMap<ConfigPath, Vec<String>>;

/// Structured table path → the original environment variable that
/// produced the **winning** node at that path.
///
/// Last-writer, same as the value table: a later var that replaces a
/// subtree drops the losing descendants. Unknown-key reporting stays on
/// [`EnvSources`].
pub(crate) type EnvWinners = BTreeMap<ConfigPath, String>;

/// Build a config value [`Map`] from environment variables matching `{PREFIX}__*`.
///
/// Double underscore `__` separates nesting levels.
/// Single `_` within a segment is literal (part of the field name).
/// Segments are lowercased to match Rust field names.
///
/// Values are parsed heuristically: bool > integer > float > string.
///
/// When the environment sets the same key both flat and nested
/// (`MYAPP__DATABASE=x` AND `MYAPP__DATABASE__URL=y`), the shapes
/// conflict and the **last-processed variable wins** — a nested var
/// replaces an earlier flat scalar with a table, and a flat var replaces
/// an earlier nested table with its scalar. Environment iteration order
/// is unspecified, so which variable is "last" is not guaranteed; the
/// semantics are last-writer, not an ordering promise. Don't mix the two
/// shapes for one key.
///
/// Takes an iterator so tests can pass synthetic data instead of
/// `std::env::vars()`. Also returns the original variable names for
/// each path ([`EnvSources`], every spelling for unknown-key errors)
/// and the last-writer map ([`EnvWinners`], provenance).
pub(crate) fn env_to_table_with_sources(
    prefix: &str,
    vars: impl IntoIterator<Item = (String, String)>,
) -> (Map, EnvSources, EnvWinners) {
    let needle = format!("{prefix}__");
    let mut table = Map::new();
    let mut sources = EnvSources::new();
    let mut winners = EnvWinners::new();

    for (key, value) in vars {
        let Some(rest) = key.strip_prefix(&needle) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }

        let segments: Vec<&str> = rest.split("__").collect();
        insert_nested(
            &mut table,
            &mut sources,
            &mut winners,
            &segments,
            parse_env_value(&value),
            ConfigPath::new(),
            &key,
        );
    }

    (table, sources, winners)
}

/// Original variable names that produced `path` or any descendant of it.
///
/// Unknown-key traversal stops at the first unknown schema ancestor, so
/// `MYAPP__DATABASE__ROGUE` is stored under `database.rogue` while
/// validation reports `database`. An exact-path lookup would miss it;
/// this walks descendants (segment prefix, not a dotted-string prefix)
/// and deduplicates names in first-seen (path-sorted, then recorded)
/// order so the error can still name every variable to unset.
pub(crate) fn env_source_names(sources: &EnvSources, path: &ConfigPath) -> Option<String> {
    let names = env_source_vars(sources, path);
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

/// Original variable names that produced `path` or any descendant, in
/// first-seen order. Empty when nothing under `path` was set.
pub(crate) fn env_source_vars(sources: &EnvSources, path: &ConfigPath) -> Vec<String> {
    let mut names = Vec::new();
    for (key, vars) in sources {
        if key == path || is_descendant(key, path) {
            for name in vars {
                if !names.iter().any(|existing| existing == name) {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

fn insert_nested(
    table: &mut Map,
    sources: &mut EnvSources,
    winners: &mut EnvWinners,
    segments: &[&str],
    value: Value,
    path: ConfigPath,
    original: &str,
) {
    debug_assert!(!segments.is_empty());

    let key = segments[0].to_lowercase();
    let child = path.key(key.clone());

    if segments.len() == 1 {
        table.insert(key, value);
        prune_descendant_winners(winners, &child);
        winners.insert(child.clone(), original.to_string());
        let entry = sources.entry(child).or_default();
        if !entry.iter().any(|name| name == original) {
            entry.push(original.to_string());
        }
    } else {
        let existed_as_map = matches!(table.get(&key), Some(Value::Map(_)));
        let sub = table.entry(key).or_insert_with(|| Value::Map(Map::new()));
        // If a flat var (e.g. MYAPP__DATABASE=x) already set this key to a
        // non-map, replace it — the more specific nested key wins.
        if !matches!(sub, Value::Map(_)) {
            *sub = Value::Map(Map::new());
            prune_descendant_winners(winners, &child);
            winners.insert(child.clone(), original.to_string());
        } else if !existed_as_map {
            winners.insert(child.clone(), original.to_string());
        }
        if let Value::Map(sub_map) = sub {
            insert_nested(
                sub_map,
                sources,
                winners,
                &segments[1..],
                value,
                child,
                original,
            );
        }
    }
}

/// Drop last-writer names under `path` when that node is replaced.
fn prune_descendant_winners(winners: &mut EnvWinners, path: &ConfigPath) {
    winners.retain(|key, _| !is_descendant(key, path));
}

/// `true` when `key` is a proper descendant of `path` (strictly longer,
/// matching segment prefix). Distinct from dotted-string prefix matches:
/// `plugins."a.config".x` is not under `plugins.a`.
fn is_descendant(key: &ConfigPath, path: &ConfigPath) -> bool {
    let segs = key.segments();
    let prefix = path.segments();
    segs.len() > prefix.len() && segs.starts_with(prefix)
}

/// Parse a string value into a typed config value.
/// Tries: bool → integer → float → string.
pub(crate) fn parse_env_value(s: &str) -> Value {
    if s.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        // Only use float if the string actually contains a dot,
        // to avoid "NaN" / "inf" being parsed as float.
        if s.contains('.') {
            return Value::Float(f);
        }
    }
    Value::String(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn env_to_table(prefix: &str, vars: impl IntoIterator<Item = (String, String)>) -> Map {
        env_to_table_with_sources(prefix, vars).0
    }

    fn env_winner(winners: &EnvWinners, path: ConfigPath) -> Option<&str> {
        winners.get(&path).map(String::as_str)
    }

    fn path(keys: &[&str]) -> ConfigPath {
        keys.iter().fold(ConfigPath::new(), |p, k| p.key(*k))
    }

    #[test]
    fn simple_key() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__HOST", "0.0.0.0")]));
        assert_eq!(table["host"].as_str().unwrap(), "0.0.0.0");
    }

    #[test]
    fn nested_key() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__DATABASE__URL", "postgres://db")]));
        let db = table["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "postgres://db");
    }

    #[test]
    fn single_underscore_preserved() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__POOL_SIZE", "10")]));
        assert_eq!(table["pool_size"].as_integer().unwrap(), 10);
    }

    #[test]
    fn parse_bool_true() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__DEBUG", "true")]));
        assert!(table["debug"].as_bool().unwrap());
    }

    #[test]
    fn parse_bool_false_case_insensitive() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__DEBUG", "FALSE")]));
        assert!(!table["debug"].as_bool().unwrap());
    }

    #[test]
    fn parse_integer() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__PORT", "8080")]));
        assert_eq!(table["port"].as_integer().unwrap(), 8080);
    }

    #[test]
    fn parse_negative_integer() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__OFFSET", "-5")]));
        assert_eq!(table["offset"].as_integer().unwrap(), -5);
    }

    #[test]
    fn parse_float() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__RATE", "1.5")]));
        assert_eq!(table["rate"].as_float().unwrap(), 1.5);
    }

    #[test]
    fn parse_string_fallback() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__NAME", "hello world")]));
        assert_eq!(table["name"].as_str().unwrap(), "hello world");
    }

    #[test]
    fn no_matching_prefix_ignored() {
        let table = env_to_table("MYAPP", vars(&[("OTHER__HOST", "x")]));
        assert!(table.is_empty());
    }

    #[test]
    fn bare_prefix_ignored() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP", "x")]));
        assert!(table.is_empty());
    }

    #[test]
    fn prefix_with_single_underscore_not_matched() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP_HOST", "x")]));
        assert!(table.is_empty());
    }

    #[test]
    fn multiple_vars_combined() {
        let table = env_to_table(
            "APP",
            vars(&[
                ("APP__HOST", "0.0.0.0"),
                ("APP__PORT", "3000"),
                ("APP__DATABASE__URL", "pg://"),
                ("APP__DATABASE__POOL_SIZE", "20"),
            ]),
        );
        assert_eq!(table["host"].as_str().unwrap(), "0.0.0.0");
        assert_eq!(table["port"].as_integer().unwrap(), 3000);
        let db = table["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "pg://");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
    }

    #[test]
    fn flat_key_replaced_by_nested() {
        // Flat vs nested on the same key is last-writer-wins (env
        // iteration order is unspecified; this iterator fixes it): here
        // the nested var is processed last, so it replaces the flat
        // scalar with a table.
        let table = env_to_table(
            "MYAPP",
            vars(&[
                ("MYAPP__DATABASE", "flat_value"),
                ("MYAPP__DATABASE__URL", "pg://"),
            ]),
        );
        let db = table["database"].as_map().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "pg://");
    }

    #[test]
    fn nested_key_then_flat_overwrites() {
        // Reverse order: nested first, then flat. The flat var is last,
        // so it replaces the table — same last-writer rule, other winner.
        let table = env_to_table(
            "MYAPP",
            vars(&[
                ("MYAPP__DATABASE__URL", "pg://"),
                ("MYAPP__DATABASE", "flat_value"),
            ]),
        );
        assert_eq!(table["database"].as_str().unwrap(), "flat_value");
    }

    #[test]
    fn mixed_case_suffix_keeps_original_variable_name() {
        // The suffix is lowercased for the table path; the recorded
        // source name is the spelling that produced the value.
        let (table, sources, _) = env_to_table_with_sources(
            "MYAPP",
            vars(&[("MYAPP__rogue_key", "1"), ("MYAPP__Database__Rogue", "x")]),
        );
        assert_eq!(table["rogue_key"].as_integer().unwrap(), 1);
        assert_eq!(
            sources.get(&path(&["rogue_key"])),
            Some(&vec!["MYAPP__rogue_key".to_string()])
        );
        let db = table["database"].as_map().unwrap();
        assert_eq!(db["rogue"].as_str().unwrap(), "x");
        assert_eq!(
            sources.get(&path(&["database", "rogue"])),
            Some(&vec!["MYAPP__Database__Rogue".to_string()])
        );
    }

    #[test]
    fn source_names_include_descendants_of_an_unknown_ancestor() {
        // Stored under the leaf path; validation reports the unknown
        // ancestor. Both the exact path and its descendants must name
        // the variable.
        let (_, sources, _) =
            env_to_table_with_sources("MYAPP", vars(&[("MYAPP__DATABASE__ROGUE", "1")]));
        assert_eq!(
            env_source_names(&sources, &path(&["database", "rogue"])).as_deref(),
            Some("MYAPP__DATABASE__ROGUE")
        );
        assert_eq!(
            env_source_names(&sources, &path(&["database"])).as_deref(),
            Some("MYAPP__DATABASE__ROGUE")
        );
        // A sibling prefix must not steal the name.
        assert_eq!(env_source_names(&sources, &path(&["data"])), None);
        assert_eq!(
            env_source_names(&sources, &path(&["database_backup"])),
            None
        );
    }

    #[test]
    fn colliding_source_names_last_win_the_value_and_keep_every_spelling() {
        // Two names collapse onto `host`. The later value wins; both
        // spellings stay on the source list so an unknown-key error
        // can name every variable to unset.
        let (table, sources, winners) = env_to_table_with_sources(
            "MYAPP",
            vars(&[("MYAPP__host", "first"), ("MYAPP__HOST", "second")]),
        );
        assert_eq!(table["host"].as_str().unwrap(), "second");
        assert_eq!(
            sources.get(&path(&["host"])),
            Some(&vec!["MYAPP__host".to_string(), "MYAPP__HOST".to_string()])
        );
        assert_eq!(env_winner(&winners, path(&["host"])), Some("MYAPP__HOST"));
    }

    #[test]
    fn winners_drop_losing_nested_var_when_flat_replaces_table() {
        let (_, sources, winners) = env_to_table_with_sources(
            "APP",
            vars(&[("APP__DATABASE__URL", "x"), ("APP__DATABASE", "oops")]),
        );
        assert_eq!(
            env_winner(&winners, path(&["database"])),
            Some("APP__DATABASE")
        );
        assert_eq!(env_winner(&winners, path(&["database", "url"])), None);
        assert!(
            env_source_names(&sources, &path(&["database"]))
                .as_deref()
                .is_some_and(|s| s.contains("APP__DATABASE__URL")),
            "unknown-key aggregation still lists the losing nested var"
        );
    }

    #[test]
    fn winners_drop_losing_flat_var_when_nested_replaces_scalar() {
        let (_, _, winners) = env_to_table_with_sources(
            "APP",
            vars(&[("APP__DATABASE", "oops"), ("APP__DATABASE__URL", "x")]),
        );
        assert_eq!(
            env_winner(&winners, path(&["database"])),
            Some("APP__DATABASE__URL")
        );
        assert_eq!(
            env_winner(&winners, path(&["database", "url"])),
            Some("APP__DATABASE__URL")
        );
    }

    #[test]
    fn dotted_map_of_entry_and_nested_path_keep_distinct_winners() {
        // `__` nests; a literal `.` is part of a segment. These two
        // flatten to the same unquoted dotted string (`plugins.a.config.x`)
        // but are distinct ConfigPaths — a MapOf entry `"a.config"` vs
        // genuinely nested `a` → `config`.
        let (table, _, winners) = env_to_table_with_sources(
            "APP",
            vars(&[
                ("APP__PLUGINS__A.CONFIG__X", "dotted"),
                ("APP__PLUGINS__A__CONFIG__X", "nested"),
            ]),
        );
        let plugins = table["plugins"].as_map().unwrap();
        assert_eq!(
            plugins["a.config"].as_map().unwrap()["x"].as_str(),
            Some("dotted")
        );
        assert_eq!(
            plugins["a"].as_map().unwrap()["config"].as_map().unwrap()["x"].as_str(),
            Some("nested")
        );
        assert_eq!(
            env_winner(&winners, path(&["plugins", "a.config", "x"])),
            Some("APP__PLUGINS__A.CONFIG__X")
        );
        assert_eq!(
            env_winner(&winners, path(&["plugins", "a", "config", "x"])),
            Some("APP__PLUGINS__A__CONFIG__X")
        );
    }

    #[test]
    fn prune_does_not_drop_a_dotted_key_that_shares_a_string_prefix() {
        // Inserting a flat `plugins.a` must not drop `plugins."a.config".x`
        // just because the unquoted displays share a `plugins.a.` prefix.
        let (table, _, winners) = env_to_table_with_sources(
            "APP",
            vars(&[
                ("APP__PLUGINS__A.CONFIG__X", "dotted"),
                ("APP__PLUGINS__A", "flat"),
            ]),
        );
        let plugins = table["plugins"].as_map().unwrap();
        assert_eq!(plugins["a"].as_str(), Some("flat"));
        assert_eq!(
            plugins["a.config"].as_map().unwrap()["x"].as_str(),
            Some("dotted")
        );
        assert_eq!(
            env_winner(&winners, path(&["plugins", "a"])),
            Some("APP__PLUGINS__A")
        );
        assert_eq!(
            env_winner(&winners, path(&["plugins", "a.config", "x"])),
            Some("APP__PLUGINS__A.CONFIG__X")
        );
    }
}
