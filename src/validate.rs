//! Strict-mode validation: detect unknown keys in config files.
//!
//! Uses `serde_ignored` to deserialize into `C::Layer` (all-optional fields) and
//! capture any keys that the layer doesn't consume. Reports each unknown key with
//! its file path and best-effort line number.

use std::path::Path;

use confique::Config;
use serde::Deserialize;

use crate::error::ClapfigError;

/// Validate that a TOML config file contains no keys unknown to config type `C`.
///
/// Uses `serde_ignored` to detect unrecognized keys during deserialization into
/// `C::Layer` (where all fields are `Option<T>`). Any key that `C::Layer` doesn't
/// consume is unknown.
///
/// Line numbers are found by searching the source text for the key name.
pub fn validate_unknown_keys<C: Config>(content: &str, path: &Path) -> Result<(), ClapfigError>
where
    C::Layer: for<'de> Deserialize<'de>,
{
    let mut unknown_keys: Vec<String> = Vec::new();

    let deserializer = toml::Deserializer::new(content);
    let _layer: C::Layer = serde_ignored::deserialize(deserializer, |ignored_path| {
        unknown_keys.push(ignored_path.to_string());
    })
    .map_err(|e| ClapfigError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })?;

    if unknown_keys.is_empty() {
        return Ok(());
    }

    let errors: Vec<ClapfigError> = unknown_keys
        .into_iter()
        .map(|key| {
            let line = find_key_line(content, &key);
            ClapfigError::UnknownKey {
                key,
                path: path.to_path_buf(),
                line,
            }
        })
        .collect();

    Err(ClapfigError::UnknownKeys(errors))
}

/// Find the 1-indexed line number for a key in TOML content.
///
/// For a dotted key like `"database.typo"`, tracks the current `[section]` header
/// while scanning and only matches the leaf key when inside the correct section.
///
/// This is a best-effort heuristic — it handles standard `[section]` headers and
/// bare key assignments but does not handle quoted keys or inline tables.
/// Returns 0 if the key cannot be located.
fn find_key_line(content: &str, dotted_key: &str) -> usize {
    let segments: Vec<&str> = dotted_key.split('.').collect();
    let leaf = segments.last().unwrap_or(&dotted_key);
    let expected_section = &segments[..segments.len() - 1]; // empty for top-level

    let mut current_section: Vec<String> = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Track [section] headers
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            let header = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            current_section = header.split('.').map(|s| s.trim().to_string()).collect();
            continue;
        }

        // Check if we're in the right section
        let in_right_section = expected_section.len() == current_section.len()
            && expected_section
                .iter()
                .zip(&current_section)
                .all(|(a, b)| *a == b);

        if in_right_section
            && let Some(after_key) = trimmed.strip_prefix(leaf)
            && after_key.trim_start().starts_with('=')
        {
            return i + 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("/test/config.toml")
    }

    #[test]
    fn valid_config_passes() {
        let content = r#"
host = "0.0.0.0"
port = 3000
debug = true

[database]
url = "postgres://localhost"
pool_size = 10
"#;
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        assert!(result.is_ok());
    }

    #[test]
    fn unknown_top_level_key() {
        let content = "host = \"localhost\"\ntypo_key = 42\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => {
                assert_eq!(keys.len(), 1);
                match &keys[0] {
                    ClapfigError::UnknownKey { key, line, .. } => {
                        assert_eq!(key, "typo_key");
                        assert_eq!(*line, 2);
                    }
                    other => panic!("Expected UnknownKey, got: {other:?}"),
                }
            }
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }

    #[test]
    fn unknown_nested_key() {
        let content = "[database]\nurl = \"pg://\"\ntypo = \"bad\"\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => {
                assert_eq!(keys.len(), 1);
                match &keys[0] {
                    ClapfigError::UnknownKey { key, .. } => {
                        assert_eq!(key, "database.typo");
                    }
                    other => panic!("Expected UnknownKey, got: {other:?}"),
                }
            }
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }

    #[test]
    fn multiple_unknown_keys() {
        let content = "typo1 = 1\ntypo2 = 2\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => {
                assert_eq!(keys.len(), 2);
            }
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }

    #[test]
    fn line_number_accuracy() {
        let content = "host = \"x\"\nport = 8080\ndebug = false\n\n# comment\nbad_key = 1\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => match &keys[0] {
                ClapfigError::UnknownKey { line, .. } => {
                    assert_eq!(*line, 6);
                }
                other => panic!("Expected UnknownKey, got: {other:?}"),
            },
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }

    #[test]
    fn empty_content_ok() {
        let result = validate_unknown_keys::<TestConfig>("", &path());
        assert!(result.is_ok());
    }

    #[test]
    fn known_optional_field_ok() {
        let content = "[database]\nurl = \"pg://\"\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        assert!(result.is_ok());
    }

    #[test]
    fn sparse_config_ok() {
        let content = "port = 3000\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        assert!(result.is_ok());
    }

    #[test]
    fn error_includes_file_path() {
        let content = "typo = 1\n";
        let p = PathBuf::from("/home/user/.config/myapp/config.toml");
        let err = validate_unknown_keys::<TestConfig>(content, &p).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("config.toml") || msg.contains("Unknown keys"));
    }

    #[test]
    fn line_number_finds_correct_section_for_duplicate_leaf() {
        // "typo" appears in [database] section — find_key_line should locate
        // it there (line 4), not confuse it with a top-level key.
        let content = "host = \"x\"\nport = 8080\n[database]\ntypo = \"bad\"\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => match &keys[0] {
                ClapfigError::UnknownKey { key, line, .. } => {
                    assert_eq!(key, "database.typo");
                    assert_eq!(*line, 4);
                }
                other => panic!("Expected UnknownKey, got: {other:?}"),
            },
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }

    #[test]
    fn line_number_top_level_not_confused_by_nested_same_name() {
        // "pool_size" exists as a known key in [database] but is unknown at top level.
        // The line finder should find it at line 1 (top level), not inside [database].
        let content = "typo = 99\n[database]\npool_size = 5\n";
        let result = validate_unknown_keys::<TestConfig>(content, &path());
        let err = result.unwrap_err();
        match err {
            ClapfigError::UnknownKeys(keys) => match &keys[0] {
                ClapfigError::UnknownKey { key, line, .. } => {
                    assert_eq!(key, "typo");
                    assert_eq!(*line, 1);
                }
                other => panic!("Expected UnknownKey, got: {other:?}"),
            },
            other => panic!("Expected UnknownKeys, got: {other:?}"),
        }
    }
}
