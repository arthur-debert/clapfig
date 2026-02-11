use toml::{Table, Value};

/// Build a `toml::Table` from environment variables matching `{PREFIX}__*`.
///
/// Double underscore `__` separates nesting levels.
/// Single `_` within a segment is literal (part of the field name).
/// Segments are lowercased to match Rust field names.
///
/// Values are parsed heuristically: bool > integer > float > string.
///
/// Takes an iterator so tests can pass synthetic data instead of `std::env::vars()`.
pub fn env_to_table(prefix: &str, vars: impl IntoIterator<Item = (String, String)>) -> Table {
    let needle = format!("{prefix}__");
    let mut table = Table::new();

    for (key, value) in vars {
        let Some(rest) = key.strip_prefix(&needle) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }

        let segments: Vec<&str> = rest.split("__").collect();
        insert_nested(&mut table, &segments, parse_env_value(&value));
    }

    table
}

fn insert_nested(table: &mut Table, segments: &[&str], value: Value) {
    debug_assert!(!segments.is_empty());

    let key = segments[0].to_lowercase();

    if segments.len() == 1 {
        table.insert(key, value);
    } else {
        let sub = table
            .entry(&key)
            .or_insert_with(|| Value::Table(Table::new()));
        if let Value::Table(sub_table) = sub {
            insert_nested(sub_table, &segments[1..], value);
        }
    }
}

/// Parse an env var value into a typed TOML value.
/// Tries: bool → integer → float → string.
fn parse_env_value(s: &str) -> Value {
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

    #[test]
    fn simple_key() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__HOST", "0.0.0.0")]));
        assert_eq!(table["host"].as_str().unwrap(), "0.0.0.0");
    }

    #[test]
    fn nested_key() {
        let table = env_to_table("MYAPP", vars(&[("MYAPP__DATABASE__URL", "postgres://db")]));
        let db = table["database"].as_table().unwrap();
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
        let db = table["database"].as_table().unwrap();
        assert_eq!(db["url"].as_str().unwrap(), "pg://");
        assert_eq!(db["pool_size"].as_integer().unwrap(), 20);
    }
}
