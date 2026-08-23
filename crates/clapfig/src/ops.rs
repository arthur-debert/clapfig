//! Config operations: template generation, key lookup, listing, and result types.
//!
//! Provides the logic behind `config list`, `config gen`, `config get`, and the
//! `ConfigResult` enum that callers use to display results.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::ClapfigError;
use crate::format::FormatAdapter;
use crate::runtime::Shape;
use crate::value::{Map, Value};

/// Result of a config operation. Returned to the caller for display.
///
/// Variants that show config data (`KeyValue`, `Listing`, `ValueSet`)
/// carry both the structured fields (for programmatic consumers) and a
/// pre-`rendered` display form spelled by the active format's adapter
/// ([`FormatAdapter::display_entry`] / [`display_comment`]) — `key = value`
/// under TOML, `key: value` under YAML, `"key": value` under JSON — which
/// is what [`Display`](fmt::Display) prints. The active format is the
/// scope file's format for scoped operations and the preferred
/// (first-enabled) format for merged views.
///
/// [`display_comment`]: FormatAdapter::display_comment
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigResult {
    /// A generated config template string, in the requested format.
    Template(String),
    /// Confirmation that a template was written to a file.
    TemplateWritten { path: PathBuf },
    /// A generated JSON Schema document (already serialized).
    Schema(String),
    /// Confirmation that a JSON Schema document was written to a file.
    SchemaWritten { path: PathBuf },
    /// A key's resolved value and its doc comment.
    KeyValue {
        key: String,
        value: String,
        doc: Vec<String>,
        /// The display block (comment lines + assignment) in the active
        /// format's spelling; what `Display` prints.
        rendered: String,
    },
    /// Confirmation that a value was persisted.
    ValueSet {
        key: String,
        value: String,
        /// The `key = value` assignment in the active format's spelling.
        rendered: String,
    },
    /// Confirmation that a value was removed.
    ValueUnset { key: String },
    /// All resolved configuration key-value pairs.
    Listing {
        entries: Vec<(String, String)>,
        /// One assignment line per entry in the active format's spelling,
        /// newline-joined; what `Display` prints.
        rendered: String,
    },
}

impl ConfigResult {
    /// Build a [`ConfigResult::KeyValue`], rendering the display block
    /// (doc-comment lines, then the assignment) through `adapter`.
    pub(crate) fn key_value(
        adapter: &dyn FormatAdapter,
        key: String,
        value: String,
        doc: Vec<String>,
    ) -> Self {
        let mut rendered = String::new();
        for line in &doc {
            rendered.push_str(&adapter.display_comment(line));
            rendered.push('\n');
        }
        rendered.push_str(&adapter.display_entry(&key, &value));
        ConfigResult::KeyValue {
            key,
            value,
            doc,
            rendered,
        }
    }

    /// Build a [`ConfigResult::ValueSet`], rendering the confirmed
    /// assignment through `adapter`.
    pub(crate) fn value_set(adapter: &dyn FormatAdapter, key: String, value: String) -> Self {
        let rendered = adapter.display_entry(&key, &value);
        ConfigResult::ValueSet {
            key,
            value,
            rendered,
        }
    }

    /// Build a [`ConfigResult::Listing`], rendering one assignment line
    /// per entry through `adapter`.
    pub(crate) fn listing(adapter: &dyn FormatAdapter, entries: Vec<(String, String)>) -> Self {
        let rendered = entries
            .iter()
            .map(|(key, value)| adapter.display_entry(key, value))
            .collect::<Vec<_>>()
            .join("\n");
        ConfigResult::Listing { entries, rendered }
    }
}

impl fmt::Display for ConfigResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigResult::Template(t) => write!(f, "{t}"),
            ConfigResult::TemplateWritten { path } => {
                write!(f, "Config template written to {}", path.display())
            }
            ConfigResult::Schema(s) => write!(f, "{s}"),
            ConfigResult::SchemaWritten { path } => {
                write!(f, "Config schema written to {}", path.display())
            }
            ConfigResult::KeyValue { rendered, .. } => write!(f, "{rendered}"),
            ConfigResult::ValueSet { rendered, .. } => write!(f, "Set {rendered}"),
            ConfigResult::ValueUnset { key } => write!(f, "Unset {key}"),
            ConfigResult::Listing { rendered, .. } => write!(f, "{rendered}"),
        }
    }
}

fn kebab_rename_fields(schema: &mut crate::runtime::Schema) {
    for nf in &mut schema.fields {
        if nf.name.contains('_') {
            nf.name = crate::normalize::kebab_key(&nf.name);
        }
        kebab_rename_shape(&mut nf.field);
    }
}

fn kebab_rename_shape(shape: &mut Shape) {
    match shape {
        Shape::Object(child) => kebab_rename_fields(child),
        Shape::Array(array) => kebab_rename_shape(&mut array.item),
        Shape::Map(map) => kebab_rename_shape(&mut map.item),
        Shape::Tagged(tagged) => {
            if tagged.tag.contains('_') {
                tagged.tag = crate::normalize::kebab_key(&tagged.tag);
            }
            for variant in &mut tagged.variants {
                kebab_rename_fields(&mut variant.schema);
            }
        }
        Shape::Leaf(_) => {}
    }
}

/// Template generator: renders the documented config template for a
/// schema through the given format adapter. With `kebab` set, the schema's
/// field names are structurally renamed to kebab-case first (see
/// [`kebab_renamed_shape`]) so the rendered keys — in any format — match what a
/// `normalize_keys(true)` builder accepts. Tagged unions rename the tag
/// key and every variant field; discriminator *values* are unchanged.
///
/// The renaming decides only what clapfig WRITES. What it ACCEPTS is
/// wider — either spelling of a multiword key — and describing that is
/// `json_schema::KeySpelling`'s job, not this one's.
///
/// With `kebab` set the shape must first be reachable under
/// normalization ([`check_shape_reachable`](crate::normalize::check_shape_reachable)):
/// a schema declaring a key that already holds a `-` gets
/// [`UnreachableNormalizedKey`](ClapfigError::UnreachableNormalizedKey)
/// rather than a template whose keys this builder's own loader would
/// reject. Without `kebab` the shape renders as declared and is passed
/// through borrowed — no rename, so no copy.
///
/// The template body — doc comments, `Allowed:` enum lines, commented
/// placeholders for defaultless leaves — is the adapter's
/// [`template`](crate::format::FormatAdapter::template) output; this
/// function is the routing seam every template consumer (`config gen`,
/// file seeding) goes through.
pub(crate) fn generate_template(
    adapter: &dyn FormatAdapter,
    shape: &Shape,
    kebab: bool,
) -> Result<String, ClapfigError> {
    if !kebab {
        return Ok(adapter.template(shape)?);
    }
    crate::normalize::check_shape_reachable(shape)
        .map_err(crate::normalize::UnreachableKey::into_error)?;
    Ok(adapter.template(&kebab_renamed_shape(shape))?)
}

/// Recursively rewrite field names in a document-root shape to kebab-case.
fn kebab_renamed_shape(shape: &Shape) -> Shape {
    let mut shaped = shape.clone();
    kebab_rename_shape(&mut shaped);
    shaped
}

/// List entries from a single scope's config file (raw file content, not merged).
///
/// The file is parsed through `adapter` — the routing seam for scoped
/// reads. If the file does not exist, returns an empty listing.
pub(crate) fn list_scope_file(
    adapter: &dyn FormatAdapter,
    file_path: &Path,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigResult::listing(adapter, Vec::new()));
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let parsed = adapter
        .parse(&content)
        .map_err(|e| ClapfigError::ParseError {
            path: file_path.to_path_buf(),
            source: Box::new(e),
            source_text: Some(std::sync::Arc::from(content.as_str())),
        })?;
    let table = match parsed.value {
        Value::Map(map) => map,
        other => {
            return Err(ClapfigError::invalid_value(
                file_path.display().to_string(),
                format!(
                    "config documents must be maps at the root, got {}",
                    other.type_str()
                ),
            ));
        }
    };

    let mut entries = Vec::new();
    flatten_value_map(&table, "", &mut entries);

    Ok(ConfigResult::listing(adapter, entries))
}

/// Recursively flatten a value map into dotted key-value pairs.
fn flatten_value_map(table: &Map, prefix: &str, entries: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Map(t) => flatten_value_map(t, &full_key, entries),
            _ => entries.push((full_key, format_value(value))),
        }
    }
}

/// Navigate a value [`Map`] by a canonical snake_case dotted path using
/// dash/underscore key equivalence. The whole document is first validated
/// with [`check_collisions`](crate::normalize::check_collisions), so a
/// table holding more than one equivalent spelling ANYWHERE — even off
/// the requested path — errs as a
/// [`KeyCollision`](crate::normalize::KeyCollision) instead of the lookup
/// answering from a document the load path refuses; each segment then
/// resolves through
/// [`resolve_table_key`](crate::normalize::resolve_table_key). The
/// raw-file counterpart of the load path's key normalization — scoped
/// `config get` reads un-normalized documents, so a normalized
/// (kebab-case) file must still answer for its canonical key. `Ok(None)`
/// when the path doesn't resolve.
pub(crate) fn table_get_normalized<'a>(
    table: &'a Map,
    canonical: &str,
) -> Result<Option<&'a Value>, crate::normalize::KeyCollision> {
    crate::normalize::check_collisions(table)?;
    let mut current = table;
    let mut segments = canonical.split('.').peekable();
    while let Some(seg) = segments.next() {
        let Some(key) = crate::normalize::resolve_table_key(current, seg) else {
            return Ok(None);
        };
        let value = &current[key];
        if segments.peek().is_none() {
            return Ok(Some(value));
        }
        match value.as_map() {
            Some(map) => current = map,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Navigate a value [`Map`] by dotted key path (e.g. `"database.url"`).
pub(crate) fn table_get<'a>(table: &'a Map, dotted_key: &str) -> Option<&'a Value> {
    let (path, leaf) = match dotted_key.rsplit_once('.') {
        Some((p, l)) => (Some(p), l),
        None => (None, dotted_key),
    };

    let tbl = match path {
        Some(path) => {
            let mut current = table;
            for segment in path.split('.') {
                current = current.get(segment)?.as_map()?;
            }
            current
        }
        None => table,
    };

    tbl.get(leaf)
}

/// Format a config value for display in listings and `config get` output.
fn format_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => crate::value::lexical_string(d),
        Value::Array(_) | Value::Map(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::test_schema;
    use crate::format::TomlAdapter;

    fn template_for(schema: &crate::runtime::Schema, kebab: bool) -> String {
        generate_template(
            &TomlAdapter,
            &crate::runtime::Shape::Object(schema.clone()),
            kebab,
        )
        .unwrap()
    }

    #[test]
    fn generate_template_contains_keys() {
        let template = template_for(&test_schema(), false);
        assert!(template.contains("host"));
        assert!(template.contains("port"));
        assert!(template.contains("database"));
        assert!(template.contains("pool_size"));
    }

    #[test]
    fn generate_template_contains_doc_comments() {
        let template = template_for(&test_schema(), false);
        assert!(template.contains("application host"));
        assert!(template.contains("port number"));
    }

    #[test]
    fn generate_template_kebab_rewrites_snake_keys() {
        let template = template_for(&test_schema(), true);
        // The nested `pool_size` field should be emitted as `pool-size`.
        assert!(
            template.contains("pool-size"),
            "expected kebab key in template:\n{template}"
        );
        assert!(
            !template.contains("pool_size"),
            "expected no snake leak in template:\n{template}"
        );
    }

    #[test]
    fn generate_template_kebab_preserves_doc_comments() {
        // Doc comments that happen to mention the snake form (in prose, not
        // as keys) should not be rewritten. The fixture's docs include
        // "Connection pool size."—lowercase plain English—but we also want
        // the structural guarantee that `# ` lines pass through verbatim.
        let template = template_for(&test_schema(), true);
        assert!(template.contains("Connection pool size."));
    }

    #[test]
    fn generate_template_kebab_off_is_default_behavior() {
        // Sanity: with the flag off, snake keys pass through untouched
        // (kebab path is opt-in).
        let raw = template_for(&test_schema(), false);
        assert!(raw.contains("pool_size"));
        assert!(!raw.contains("pool-size"));
    }

    // -- kebab rename (structural, format-agnostic) -------------------------

    #[test]
    fn kebab_rename_leaves_values_and_prose_alone() {
        // Only field NAMES are renamed — a string default keeps its
        // underscores, and doc prose mentioning the snake form survives.
        use crate::runtime::{Field as RtField, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field(
                "db_url",
                RtField::string()
                    .doc("Set db_url to a postgres URL.")
                    .default("postgres://my_user@host"),
            )
            .build();
        let template = generate_template(
            &TomlAdapter,
            &crate::runtime::Shape::Object(schema.clone()),
            true,
        )
        .unwrap();
        assert!(template.contains("db-url = "), "{template}");
        assert!(
            template.contains(r#""postgres://my_user@host""#),
            "{template}"
        );
        assert!(
            template.contains("Set db_url to a postgres URL."),
            "{template}"
        );
    }

    #[test]
    fn kebab_renames_toml_section_headers_and_commented_defaults() {
        // Nested objects become renamed [section] headers; a defaultless
        // leaf's commented placeholder carries the renamed key too.
        use crate::runtime::{Field as RtField, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .nested(
                "my_section",
                RtSchema::object("Section").field("api_key", RtField::string()),
            )
            .build();
        let template = generate_template(
            &TomlAdapter,
            &crate::runtime::Shape::Object(schema.clone()),
            true,
        )
        .unwrap();
        assert!(template.contains("[my-section]"), "{template}");
        assert!(template.contains("#api-key"), "{template}");
        assert!(!template.contains('_'), "no snake leak:\n{template}");
    }

    #[test]
    fn kebab_renames_json_template_keys_at_every_depth() {
        // The rename is structural, so it reaches JSON object keys, the
        // "//field" comment keys, and the assignment snippet inside a
        // defaultless field's comment — none of which a TOML-line textual
        // rewrite could see.
        use crate::format::json::JsonAdapter;
        use crate::runtime::{Field as RtField, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field("api_key", RtField::string().doc("Required API key."))
            .nested(
                "my_section",
                RtSchema::object("Section").field("pool_size", RtField::integer().default(5i64)),
            )
            .build();
        let template = generate_template(
            &JsonAdapter,
            &crate::runtime::Shape::Object(schema.clone()),
            true,
        )
        .unwrap();
        assert!(template.contains(r#""//api-key""#), "{template}");
        assert!(
            template.contains(r#"\"api-key\": \"\""#),
            "snippet:\n{template}"
        );
        assert!(template.contains(r#""my-section""#), "{template}");
        assert!(template.contains(r#""pool-size": 5"#), "{template}");
        assert!(!template.contains("api_key"), "no snake leak:\n{template}");
        assert!(
            !template.contains("pool_size"),
            "no snake leak:\n{template}"
        );
    }

    #[test]
    fn table_get_flat() {
        let table = crate::fixtures::test::parse_toml("port = 8080");
        let val = table_get(&table, "port").unwrap();
        assert_eq!(val.as_integer().unwrap(), 8080);
    }

    #[test]
    fn table_get_nested() {
        let table = crate::fixtures::test::parse_toml("[database]\npool_size = 5");
        let val = table_get(&table, "database.pool_size").unwrap();
        assert_eq!(val.as_integer().unwrap(), 5);
    }

    #[test]
    fn table_get_missing() {
        let table = crate::fixtures::test::parse_toml("port = 8080");
        assert!(table_get(&table, "nope").is_none());
    }

    #[test]
    fn listing_display_format() {
        let result = ConfigResult::listing(
            &TomlAdapter,
            vec![
                ("host".into(), "localhost".into()),
                ("port".into(), "8080".into()),
            ],
        );
        let display = format!("{result}");
        assert_eq!(display, "host = localhost\nport = 8080");
    }

    #[test]
    fn listing_display_is_format_aware() {
        // The active format spells the assignment lines: TOML `k = v`,
        // YAML `k: v`, JSON `"k": v`.
        use crate::format::{JsonAdapter, YamlAdapter};
        let entries = |a: &dyn FormatAdapter| {
            format!(
                "{}",
                ConfigResult::listing(a, vec![("database.url".into(), "pg://".into())])
            )
        };
        assert_eq!(entries(&TomlAdapter), "database.url = pg://");
        assert_eq!(entries(&YamlAdapter), "database.url: pg://");
        assert_eq!(entries(&JsonAdapter), "\"database.url\": pg://");
    }

    #[test]
    fn key_value_display_is_format_aware() {
        // Doc lines follow the format's comment spelling; JSON (no real
        // comments) uses its `//` convention.
        use crate::format::{JsonAdapter, YamlAdapter};
        let get = |a: &dyn FormatAdapter| {
            format!(
                "{}",
                ConfigResult::key_value(a, "port".into(), "8080".into(), vec!["The port.".into()])
            )
        };
        assert_eq!(get(&TomlAdapter), "# The port.\nport = 8080");
        assert_eq!(get(&YamlAdapter), "# The port.\nport: 8080");
        assert_eq!(get(&JsonAdapter), "// The port.\n\"port\": 8080");
    }

    #[test]
    fn value_set_display_is_format_aware() {
        use crate::format::YamlAdapter;
        let result = ConfigResult::value_set(&YamlAdapter, "port".into(), "8080".into());
        assert_eq!(format!("{result}"), "Set port: 8080");
    }

    // --- scope file operations ---

    #[test]
    fn list_scope_file_returns_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 3000\nhost = \"localhost\"\n").unwrap();

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
        match result {
            ConfigResult::Listing { entries, .. } => {
                assert_eq!(entries.len(), 2);
                assert!(entries.contains(&("host".into(), "localhost".into())));
                assert!(entries.contains(&("port".into(), "3000".into())));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_scope_file_nested() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[database]\npool_size = 10\nurl = \"pg://\"\n").unwrap();

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
        match result {
            ConfigResult::Listing { entries, .. } => {
                assert!(entries.contains(&("database.pool_size".into(), "10".into())));
                assert!(entries.contains(&("database.url".into(), "pg://".into())));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn list_scope_file_missing_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = list_scope_file(&TomlAdapter, &path).unwrap();
        match result {
            ConfigResult::Listing { entries, .. } => assert!(entries.is_empty()),
            other => panic!("Expected empty Listing, got {other:?}"),
        }
    }
}
