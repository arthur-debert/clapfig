//! Accessors for static metadata about a `Config` struct.
//!
//! Walks the confique [`Meta`](confique::meta::Meta) tree to answer questions
//! you'd otherwise need to run the full resolve pipeline (or generate a JSON
//! Schema) to get. The functions here are pure, take no I/O, and key off the
//! `C::META` constant baked in at derive time, so they're cheap enough to
//! call from inside help text, tooltip generators, settings UIs, or
//! `--describe` flags.
//!
//! # Lenient key spelling
//!
//! Lookups accept either kebab-case (`pool-size`) or snake_case (`pool_size`)
//! spelling and return the same answer either way. This is independent of
//! whether the builder has `.normalize_keys(true)` set — it just makes
//! metadata lookups DWIM-friendly so callers don't have to remember which
//! shape the user typed.

use confique::Config;

use crate::spec::{FieldKindRef, SchemaRef};

/// Look up the doc-comment lines for a config key.
///
/// `key` is a dotted path through the config struct's fields (e.g.
/// `"host"`, `"database.url"`, `"server.tls.cert_path"`). Dashes and
/// underscores in segment names are treated as equivalent, so both
/// `"database.pool-size"` and `"database.pool_size"` find the same field.
///
/// Returns:
/// - `Some(lines)` when the key resolves to a leaf or nested field. `lines`
///   is each `///` doc-comment line with its leading `/// ` stripped — the
///   same shape confique exposes in [`Meta::doc`](confique::meta::Meta::doc)
///   and `Field::doc`. An empty `Vec` means the field exists but has no
///   doc comment.
/// - `None` when no field matches that dotted path. Use this to distinguish
///   "key doesn't exist" from "key exists, undocumented."
///
/// # Example
///
/// ```ignore
/// use clapfig::meta::doc_for;
///
/// let lines = doc_for::<AppConfig>("database.pool-size")
///     .unwrap_or_default();
/// for line in lines {
///     println!("# {line}");
/// }
/// ```
pub fn doc_for<C: Config>(key: &str) -> Option<Vec<String>> {
    walk(SchemaRef::from_meta(&C::META), key)
}

pub(crate) fn walk(schema: SchemaRef<'_>, dotted_key: &str) -> Option<Vec<String>> {
    let segments: Vec<&str> = dotted_key.split('.').collect();
    walk_segments(schema, &segments)
}

fn walk_segments(schema: SchemaRef<'_>, segments: &[&str]) -> Option<Vec<String>> {
    if segments.is_empty() {
        return None;
    }
    let head = segments[0];
    for field in schema.fields() {
        if segment_matches(field.name, head) {
            if segments.len() == 1 {
                return Some(field.doc.iter().map(|s| s.to_string()).collect());
            }
            return match field.kind {
                FieldKindRef::Nested { schema: nested } => walk_segments(nested, &segments[1..]),
                // Hit a leaf with segments still pending — the rest of the
                // path can't resolve.
                FieldKindRef::Leaf(_) => None,
            };
        }
    }
    None
}

/// Treat `-` and `_` as the same character when comparing a META field name
/// against a caller-supplied segment. Field names from confique are snake by
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
    use crate::fixtures::test::TestConfig;
    use serde::{Deserialize, Serialize};

    /// Local fixture: a config struct with at least one field that has no
    /// `///` doc comment. Used to lock down the
    /// `Some(empty Vec)` vs `None` distinction in the public API.
    #[derive(confique::Config, Serialize, Deserialize, Debug)]
    struct PartiallyDocumentedConfig {
        /// This one has a doc comment.
        #[config(default = "x")]
        documented: String,

        // Intentionally no `///` line — confique will emit an empty doc slice.
        #[config(default = 0)]
        undocumented: u32,
    }

    #[test]
    fn undocumented_field_returns_some_empty_vec() {
        // The contract: existing-but-undocumented fields return Some(vec![]),
        // not None. Callers depend on this to tell "no such key" apart from
        // "key exists, no doc to show."
        let doc = doc_for::<PartiallyDocumentedConfig>("undocumented")
            .expect("field exists, even without a doc comment");
        assert!(doc.is_empty(), "expected empty doc vec, got {doc:?}");
    }

    #[test]
    fn documented_sibling_in_same_fixture_still_returns_lines() {
        // Sanity check that the partial fixture still attaches docs to the
        // documented field — guards against a regression where both fields
        // collapse to empty.
        let doc =
            doc_for::<PartiallyDocumentedConfig>("documented").expect("documented field exists");
        assert!(doc.iter().any(|line| line.contains("doc comment")));
    }

    #[test]
    fn flat_key_returns_doc() {
        let doc = doc_for::<TestConfig>("host").expect("host exists");
        assert!(doc.iter().any(|line| line.contains("application host")));
    }

    #[test]
    fn nested_key_returns_doc() {
        let doc = doc_for::<TestConfig>("database.pool_size").expect("pool_size exists");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn missing_top_level_key_returns_none() {
        assert!(doc_for::<TestConfig>("nonexistent").is_none());
    }

    #[test]
    fn missing_nested_key_returns_none() {
        assert!(doc_for::<TestConfig>("database.nonexistent").is_none());
    }

    #[test]
    fn extra_segments_past_leaf_return_none() {
        // `host` is a leaf — "host.anything" should not resolve.
        assert!(doc_for::<TestConfig>("host.anything").is_none());
    }

    #[test]
    fn empty_key_returns_none() {
        // An empty string splits to a single empty segment, which can't
        // match any field name.
        assert!(doc_for::<TestConfig>("").is_none());
    }

    #[test]
    fn kebab_spelling_finds_snake_field() {
        // Bridge case: TestDbConfig has a `pool_size` field. A caller using
        // kebab spelling should still find it.
        let doc = doc_for::<TestConfig>("database.pool-size").expect("kebab spelling resolves");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn snake_spelling_finds_snake_field() {
        let doc = doc_for::<TestConfig>("database.pool_size").expect("snake spelling resolves");
        assert!(doc.iter().any(|line| line.contains("Connection pool size")));
    }

    #[test]
    fn nested_field_section_doc() {
        // Asking for the section itself ("database") yields the section's
        // own doc comment (it has one: "Database settings.").
        let doc = doc_for::<TestConfig>("database").expect("section exists");
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
