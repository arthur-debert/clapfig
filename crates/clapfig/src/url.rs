//! Convert URL query parameters into config overrides for merging.
//!
//! Query parameters use dotted keys for nesting (`database.url=pg://`) and the
//! same heuristic value parsing as environment variables (bool > int > float >
//! string). Values are percent-decoded before parsing.

use percent_encoding::percent_decode_str;
use toml::Value;

use crate::env::parse_env_value;

/// Parse a URL query string into `(dotted_key, Value)` pairs.
///
/// Keys use `.` for nesting (same as CLI overrides). Values are
/// percent-decoded, then parsed with the same bool/int/float/string heuristic
/// used for env vars.
///
/// A leading `?` is stripped if present. Empty keys and bare `&` separators are
/// silently skipped.
///
/// ```ignore
/// let overrides = query_to_overrides("port=9090&database.url=pg%3A%2F%2Fprod&debug=true");
/// // [("port", Integer(9090)), ("database.url", String("pg://prod")), ("debug", Boolean(true))]
/// ```
pub fn query_to_overrides(query: &str) -> Vec<(String, Value)> {
    let query = query.strip_prefix('?').unwrap_or(query);
    let mut overrides = Vec::new();

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }

        let (raw_key, raw_value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };

        let key = percent_decode_str(raw_key).decode_utf8_lossy().into_owned();

        if key.is_empty() {
            continue;
        }

        let value = percent_decode_str(raw_value)
            .decode_utf8_lossy()
            .into_owned();

        overrides.push((key, parse_env_value(&value)));
    }

    overrides
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_string() {
        let o = query_to_overrides("host=example.com");
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].0, "host");
        assert_eq!(o[0].1, Value::String("example.com".into()));
    }

    #[test]
    fn integer_value() {
        let o = query_to_overrides("port=8080");
        assert_eq!(o[0].1, Value::Integer(8080));
    }

    #[test]
    fn bool_value() {
        let o = query_to_overrides("debug=true");
        assert_eq!(o[0].1, Value::Boolean(true));
    }

    #[test]
    fn float_value() {
        let o = query_to_overrides("rate=1.5");
        assert_eq!(o[0].1, Value::Float(1.5));
    }

    #[test]
    fn nested_dotted_key() {
        let o = query_to_overrides("database.url=pg://prod");
        assert_eq!(o[0].0, "database.url");
        assert_eq!(o[0].1, Value::String("pg://prod".into()));
    }

    #[test]
    fn percent_decoding() {
        let o = query_to_overrides("database.url=pg%3A%2F%2Fprod%3Fssl%3Dtrue");
        assert_eq!(o[0].1, Value::String("pg://prod?ssl=true".into()));
    }

    #[test]
    fn leading_question_mark_stripped() {
        let o = query_to_overrides("?port=3000");
        assert_eq!(o[0].0, "port");
        assert_eq!(o[0].1, Value::Integer(3000));
    }

    #[test]
    fn multiple_params() {
        let o = query_to_overrides("host=x&port=3000&debug=true");
        assert_eq!(o.len(), 3);
        assert_eq!(o[0].0, "host");
        assert_eq!(o[1].0, "port");
        assert_eq!(o[2].0, "debug");
    }

    #[test]
    fn empty_value() {
        let o = query_to_overrides("key=");
        assert_eq!(o[0].1, Value::String("".into()));
    }

    #[test]
    fn no_equals_sign() {
        let o = query_to_overrides("flag");
        assert_eq!(o[0].0, "flag");
        assert_eq!(o[0].1, Value::String("".into()));
    }

    #[test]
    fn empty_string() {
        let o = query_to_overrides("");
        assert!(o.is_empty());
    }

    #[test]
    fn bare_ampersands_skipped() {
        let o = query_to_overrides("&&port=80&&");
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].0, "port");
    }

    #[test]
    fn empty_key_skipped() {
        let o = query_to_overrides("=value");
        assert!(o.is_empty());
    }

    #[test]
    fn last_value_wins_for_duplicate_keys() {
        let o = query_to_overrides("port=3000&port=5000");
        assert_eq!(o.len(), 2);
        // Both are collected — the resolve pipeline handles last-wins via overrides_to_table
        assert_eq!(o[0].1, Value::Integer(3000));
        assert_eq!(o[1].1, Value::Integer(5000));
    }

    #[test]
    fn percent_encoded_key() {
        let o = query_to_overrides("database%2Eurl=pg://");
        assert_eq!(o[0].0, "database.url");
    }
}
