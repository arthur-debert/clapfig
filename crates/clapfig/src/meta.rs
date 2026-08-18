//! Accessors for metadata about a config schema.
//!
//! Walks a [`Schema`] to answer questions you'd
//! otherwise need to run the full resolve pipeline (or generate a JSON
//! Schema) to get. The functions here are pure and take no I/O, so they're
//! cheap enough to call from inside help text, tooltip generators, settings
//! UIs, or `--describe` flags. Derive users pass `C::schema()` (the cached
//! runtime view the `#[derive(clapfig::Schema)]` macro maintains).
//!
//! # Lenient key spelling
//!
//! Lookups accept either kebab-case (`pool-size`) or snake_case (`pool_size`)
//! spelling and return the same answer either way. This is independent of
//! whether the builder has `.normalize_keys(true)` set — it just makes
//! metadata lookups DWIM-friendly so callers don't have to remember which
//! shape the user typed.

use crate::runtime::{Field, Schema};

/// Look up the doc-comment lines for a config key in a [`Schema`].
///
/// `key` is a dotted path through the schema's fields (e.g. `"host"`,
/// `"database.url"`, `"server.tls.cert_path"`). Dashes and underscores in
/// segment names are treated as equivalent, so both `"database.pool-size"`
/// and `"database.pool_size"` find the same field.
///
/// Returns:
/// - `Some(lines)` when the key resolves to a leaf or nested field. `lines`
///   is each doc line (for derive users, each `///` doc-comment line with
///   its leading `/// ` stripped). An empty `Vec` means the field exists
///   but has no doc comment.
/// - `None` when no field matches that dotted path. Use this to distinguish
///   "key doesn't exist" from "key exists, undocumented."
///
/// # Example
///
/// ```ignore
/// use clapfig::meta::doc_for;
///
/// let lines = doc_for(AppConfig::schema(), "database.pool-size")
///     .unwrap_or_default();
/// for line in lines {
///     println!("# {line}");
/// }
/// ```
pub fn doc_for(schema: &Schema, key: &str) -> Option<Vec<String>> {
    let segments: Vec<&str> = key.split('.').collect();
    walk_segments(schema, &segments)
}

fn walk_segments(schema: &Schema, segments: &[&str]) -> Option<Vec<String>> {
    if segments.is_empty() {
        return None;
    }
    let head = segments[0];
    for field in &schema.fields {
        if segment_matches(&field.name, head) {
            if segments.len() == 1 {
                return Some(match &field.field {
                    Field::Leaf(leaf) => leaf.doc.clone(),
                    Field::Nested(s) | Field::ArrayOf(s) | Field::MapOf(s) => s.doc.clone(),
                });
            }
            return match &field.field {
                Field::Nested(nested) | Field::ArrayOf(nested) | Field::MapOf(nested) => {
                    walk_segments(nested, &segments[1..])
                }
                // Hit a leaf with segments still pending — the rest of the
                // path can't resolve.
                Field::Leaf(_) => None,
            };
        }
    }
    None
}

/// Treat `-` and `_` as the same character when comparing a schema field
/// name against a caller-supplied segment. Field names are snake by
/// convention, but callers may type kebab when their app uses
/// `.normalize_keys(true)`.
fn segment_matches(field_name: &str, caller_segment: &str) -> bool {
    if field_name == caller_segment {
        return true;
    }
    if field_name.len() != caller_segment.len() {
        return false;
    }
    field_name
        .bytes()
        .zip(caller_segment.bytes())
        .all(|(f, c)| f == c || (f == b'_' && c == b'-') || (f == b'-' && c == b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;
    use crate::runtime::{Field, Schema};

    fn doc_for_key(key: &str) -> Option<Vec<String>> {
        doc_for(&test_schema(), key)
    }

    /// Local fixture: a schema with at least one field that has no doc
    /// comment. Used to lock down the `Some(empty Vec)` vs `None`
    /// distinction in the public API.
    fn partially_documented_schema() -> Schema {
        Schema::object("PartiallyDocumentedConfig")
            .field(
                "documented",
                Field::string()
                    .doc("This one has a doc comment.")
                    .default("x"),
            )
            // Intentionally no doc line — an empty doc slice results.
            .field("undocumented", Field::integer().default(0i64))
            .build()
    }

    #[test]
    fn undocumented_field_returns_some_empty_vec() {
        // The contract: existing-but-undocumented fields return Some(vec![]),
        // not None. Callers depend on this to tell "no such key" apart from
        // "key exists, no doc to show."
        let doc = doc_for(&partially_documented_schema(), "undocumented")
            .expect("field exists, even without a doc comment");
        assert!(doc.is_empty(), "expected empty doc vec, got {doc:?}");
    }

    #[test]
    fn documented_sibling_in_same_fixture_still_returns_lines() {
        // Sanity check that the partial fixture still attaches docs to the
        // documented field — guards against a regression where both fields
        // collapse to empty.
        let doc =
            doc_for(&partially_documented_schema(), "documented").expect("documented field exists");
        assert!(doc.iter().any(|line| line.contains("doc comment")));
    }

    #[test]
    fn flat_key_returns_doc() {
        let doc = doc_for_key("host").expect("host exists");
        assert!(doc.iter().any(|line| line.contains("application host")));
    }

    #[test]
    fn nested_key_returns_doc() {
        let doc = doc_for_key("database.pool_size").expect("pool_size exists");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn missing_top_level_key_returns_none() {
        assert!(doc_for_key("nonexistent").is_none());
    }

    #[test]
    fn missing_nested_key_returns_none() {
        assert!(doc_for_key("database.nonexistent").is_none());
    }

    #[test]
    fn extra_segments_past_leaf_return_none() {
        // `host` is a leaf — "host.anything" should not resolve.
        assert!(doc_for_key("host.anything").is_none());
    }

    #[test]
    fn empty_key_returns_none() {
        // An empty string splits to a single empty segment, which can't
        // match any field name.
        assert!(doc_for_key("").is_none());
    }

    #[test]
    fn kebab_spelling_finds_snake_field() {
        // Bridge case: the fixture has a `pool_size` field. A caller using
        // kebab spelling should still find it.
        let doc = doc_for_key("database.pool-size").expect("kebab spelling resolves");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn snake_spelling_finds_snake_field() {
        let doc = doc_for_key("database.pool_size").expect("snake spelling resolves");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn nested_field_section_doc() {
        // Asking for the section itself ("database") yields the section's
        // own doc comment (it has one: "Database settings.").
        let doc = doc_for_key("database").expect("section exists");
        assert!(doc.iter().any(|line| line.contains("Database settings")));
    }

    #[test]
    fn segment_matches_treats_dash_and_underscore_equivalent() {
        assert!(segment_matches("pool_size", "pool-size"));
        assert!(segment_matches("pool_size", "pool_size"));
        assert!(segment_matches("foo-bar", "foo_bar"));
        assert!(!segment_matches("pool_size", "pool"));
        assert!(!segment_matches("pool_size", "pool_zize"));
    }
}
