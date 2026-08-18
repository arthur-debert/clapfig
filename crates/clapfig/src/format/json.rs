//! JSON format adapter — `serde_json` behind the adapter contract.
//!
//! This file is the ONLY place in the crate that gives `serde_json` a
//! *format* role (the crate also serves JSON Schema export, which is not a
//! config format). It implements the full ADR-0002 matrix row set for
//! JSON, with no known operation-level refusals:
//!
//! - [`parse`](JsonAdapter::parse): JSON text → owned [`Value`] tree, with
//!   the `"//"` comment-key convention applied: every `//`-prefixed member
//!   is format syntax owned by this adapter and is stripped at parse time,
//!   before the core [`Value`] tree exists — exactly as TOML's `#`
//!   comments never reach the tree. The stripped namespace is reserved at
//!   any nesting depth; a `//`-prefixed member can never be a
//!   configuration key.
//! - [`serialize`](JsonAdapter::serialize): [`Value`] tree → pretty JSON
//!   text. Non-finite floats have no JSON literal and refuse with a typed
//!   error naming the offending path; so does a map key in the reserved
//!   `//` namespace — writing one would produce text the next parse
//!   silently strips (the same refusal guards template field names and
//!   edit paths). Datetimes serialize as strings in their TOML lexical
//!   form (the schema-driven coercion pass reads them back, ADR-0001).
//! - [`template`](JsonAdapter::template): the documented config template,
//!   carrying documentation as comment keys — at most one `"//"` per
//!   object, an array of strings for multi-line prose, and suffixed
//!   `"//field-name"` keys for per-field docs. Defaultless leaves show
//!   their assignment snippet inside the comment (JSON cannot comment out
//!   a real key). The exported JSON Schema allowlists the `^//` pattern
//!   so third-party validators accept the generated template — clapfig's
//!   own validation never sees the keys (they are stripped at parse).
//! - [`edit`](JsonAdapter::edit): set/unset against existing source text.
//!   Comments are data in this convention, so they survive edits for
//!   free. **Formatting is normalized** (pretty-printed, two-space
//!   indent, trailing newline) — the documented, expected behavior;
//!   document key order is preserved, so comment keys stay adjacent to
//!   the fields they document, and a newly created key whose `//key`
//!   comment already exists lands right after that comment.
//!
//! Baseline mapping rules applied at parse (ADR-0002's table): `null` is
//! a typed error naming the key ("absence expresses unset"); an integer
//! literal outside `i64` — on either side, above `i64::MAX` or below
//! `i64::MIN` — is a typed error naming the key, never a silent float
//! (`serde_json`'s `arbitrary_precision` keeps the lexical form that
//! makes the two sides distinguishable from float literals). A float
//! literal overflowing `f64` is likewise a typed error. Whitespace-only
//! source parses as the empty map (an empty config file is "no config",
//! matching TOML's empty document).

use serde_json::{Map as JsonMap, Value as Json};

use crate::runtime::{Field, Leaf, LeafType, Schema};
use crate::value::{Map, Value};

use std::collections::BTreeMap;

use super::template::{TemplateRenderer, doc_lines, leaf_annotations, walk_level};
use super::{
    ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, PathSegment, Span,
    UnsupportedByFormat, WalkSegment, walk_label,
};

/// The canonical format name used in error messages.
const FORMAT: &str = "json";

/// The reserved comment-key namespace (ADR-0002): any object member whose
/// key starts with this prefix is a comment, at any nesting depth.
const COMMENT_PREFIX: &str = "//";

/// The JSON format behind the adapter contract.
///
/// Declares every ADR-0002 matrix row (JSON has no refusal rows). The one
/// gap is [`span_index`](JsonAdapter::span_index) — undeclared and
/// refused typed until the provenance epic builds the index. See the
/// [module docs](self) for the comment-key convention and the baseline
/// mapping rules this adapter applies.
pub struct JsonAdapter;

impl FormatAdapter for JsonAdapter {
    fn name(&self) -> &'static str {
        FORMAT
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn capabilities(&self) -> &'static [Operation] {
        // The provenance epic adds Operation::SpanIndex when it
        // implements span_index.
        &[
            Operation::Parse,
            Operation::Template,
            Operation::Serialize,
            Operation::EditSet,
            Operation::EditCreateKey,
            Operation::EditCreateFile,
            Operation::EditUnset,
        ]
    }

    fn parse(&self, text: &str) -> Result<Value, FormatError> {
        // An empty (or whitespace-only) file is "no config", matching
        // TOML's empty document — not a JSON syntax error.
        if text.trim().is_empty() {
            return Ok(Value::Map(Map::new()));
        }
        let json: Json = serde_json::from_str(text).map_err(|e| syntax_error(text, &e))?;
        let mut path = Vec::new();
        json_to_value(json, &mut path)
    }

    fn serialize(&self, value: &Value) -> Result<String, FormatError> {
        let mut path = Vec::new();
        let json = value_to_json(value, &mut path)?;
        Ok(render(&json))
    }

    fn template(&self, schema: &Schema) -> Result<String, FormatError> {
        let mut object = JsonMap::new();
        walk_level(&mut JsonTemplate, schema, &(), &mut object)?;
        Ok(render(&Json::Object(object)))
    }

    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        // A missing file is seeded from the template before the edit
        // reaches this adapter, but direct callers may hand an empty
        // document — start those from the empty object.
        let mut doc: Json = if source.trim().is_empty() {
            Json::Object(JsonMap::new())
        } else {
            serde_json::from_str(source).map_err(|e| syntax_error(source, &e))?
        };
        match edit {
            FileEdit::Set { path, value, .. } => {
                let keys = key_segments(path)?;
                let mut value_path: Vec<WalkSegment> = path
                    .segments()
                    .iter()
                    .map(|PathSegment::Key(k)| WalkSegment::Key(k.clone()))
                    .collect();
                let json_value = value_to_json(value, &mut value_path)?;
                super::edit::write_at_path(&mut doc, &keys, json_value)?;
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path)?;
                super::edit::unset_at_path(&mut doc, &keys);
            }
        }
        Ok(render(&doc))
    }

    fn span_index(&self, _text: &str) -> Result<BTreeMap<ConfigPath, Span>, FormatError> {
        // Provenance epic: build the path → span index from parser spans.
        Err(UnsupportedByFormat {
            format: self.name(),
            operation: Operation::SpanIndex,
        }
        .into())
    }
}

/// Pretty-print a JSON document the way every entry point emits text:
/// two-space indent plus a trailing newline. This is the formatting
/// normalization the edit capability documents.
fn render(json: &Json) -> String {
    let mut out =
        serde_json::to_string_pretty(json).expect("serde_json::Value serialization is infallible");
    out.push('\n');
    out
}

/// The refusal message for a `//`-prefixed name at an outgoing boundary
/// (serialize, template emission, edit path): the reserved comment
/// namespace (ADR-0002) means the next parse would strip such a member
/// silently, so every writer refuses it typed instead.
fn reserved_key_message(at: &str) -> String {
    format!(
        "key {at} is in the reserved JSON comment namespace: a `//`-prefixed member is comment syntax (ADR-0002) and would be stripped at the next parse — rename the key"
    )
}

/// Wrap a `serde_json` syntax error as the typed parse error, translating
/// its line/column position into a byte [`Span`] so renderers can draw
/// snippets.
fn syntax_error(text: &str, error: &serde_json::Error) -> FormatError {
    FormatError::Parse {
        format: FORMAT,
        message: error.to_string(),
        span: span_at(text, error.line(), error.column()),
    }
}

/// Byte span for a one-based line/column position in `text`. Returns
/// `None` when the position cannot be located (e.g. line 0).
///
/// `serde_json` reports the column as a one-based BYTE offset into the
/// line, so it is used as such — clamped to the line, then snapped to
/// UTF-8 character boundaries so slicing `text[span.start..span.end]`
/// can never split a multi-byte character.
fn span_at(text: &str, line: usize, column: usize) -> Option<Span> {
    if line == 0 {
        return None;
    }
    let mut offset = 0usize;
    for (i, line_text) in text.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            let byte_in_line = column.saturating_sub(1).min(line_text.len());
            let start = text.floor_char_boundary(offset + byte_in_line);
            let end = text.ceil_char_boundary(start + 1);
            return Some(Span { start, end });
        }
        offset += line_text.len();
    }
    None
}

/// Convert a parsed `serde_json::Value` into the owned model, applying the
/// baseline mapping rules: `//`-prefixed members are stripped (before the
/// [`Value`] tree exists), `null` and out-of-`i64` integers are typed
/// errors naming the offending path.
fn json_to_value(json: Json, path: &mut Vec<WalkSegment>) -> Result<Value, FormatError> {
    Ok(match json {
        Json::Null => {
            return Err(FormatError::Parse {
                format: FORMAT,
                message: format!(
                    "null at {} is not a configuration value: absence expresses unset — omit the key instead",
                    walk_label(path)
                ),
                span: None,
            });
        }
        Json::Bool(b) => Value::Boolean(b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                // `arbitrary_precision` keeps the number's lexical form, so
                // an integer literal (no fraction, no exponent) that did
                // not fit i64 is detectable on BOTH sides of the range —
                // above i64::MAX and below i64::MIN alike — and is a typed
                // error, never a silent float.
                let lexeme = n.as_str();
                if !lexeme.contains(['.', 'e', 'E']) {
                    return Err(FormatError::Parse {
                        format: FORMAT,
                        message: format!(
                            "integer {lexeme} at {} is out of range: the value model's integers are 64-bit signed (i64)",
                            walk_label(path)
                        ),
                        span: None,
                    });
                }
                match n.as_f64() {
                    Some(f) if f.is_finite() => Value::Float(f),
                    // A float literal whose magnitude overflows f64
                    // (e.g. 1e999): out of range, not silently infinity.
                    _ => {
                        return Err(FormatError::Parse {
                            format: FORMAT,
                            message: format!(
                                "float {lexeme} at {} is out of range: the value model's floats are 64-bit (f64)",
                                walk_label(path)
                            ),
                            span: None,
                        });
                    }
                }
            }
        }
        Json::String(s) => Value::String(s),
        Json::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                path.push(WalkSegment::Index(i));
                let converted = json_to_value(item, path)?;
                path.pop();
                out.push(converted);
            }
            Value::Array(out)
        }
        Json::Object(entries) => {
            let mut map = Map::new();
            for (key, entry) in entries {
                // The reserved comment namespace: stripped whole,
                // whatever the member's value shape — even a null comment
                // value is comment syntax, not a configuration value.
                if key.starts_with(COMMENT_PREFIX) {
                    continue;
                }
                path.push(WalkSegment::Key(key.clone()));
                let converted = json_to_value(entry, path)?;
                path.pop();
                map.insert(key, converted);
            }
            Value::Map(map)
        }
    })
}

/// Convert an owned [`Value`] into a `serde_json::Value`. The one
/// unrepresentable shape is a non-finite float (JSON has no literal for
/// it) — a typed error naming the offending path.
fn value_to_json(value: &Value, path: &mut Vec<WalkSegment>) -> Result<Json, FormatError> {
    Ok(match value {
        Value::String(s) => Json::String(s.clone()),
        Value::Integer(i) => Json::Number((*i).into()),
        Value::Float(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => Json::Number(n),
            None => {
                return Err(FormatError::Serialize {
                    format: FORMAT,
                    message: format!(
                        "non-finite float {f} at {} has no JSON representation",
                        walk_label(path)
                    ),
                });
            }
        },
        Value::Boolean(b) => Json::Bool(*b),
        // TOML lexical form as a string; the schema-driven coercion pass
        // reads it back into the Datetime variant (ADR-0001). The
        // panic-free spelling, not upstream `Display` (see the
        // `value::datetime` module docs).
        Value::Datetime(d) => Json::String(crate::value::lexical_string(d)),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                path.push(WalkSegment::Index(i));
                let converted = value_to_json(item, path)?;
                path.pop();
                out.push(converted);
            }
            Json::Array(out)
        }
        Value::Map(map) => {
            let mut out = JsonMap::new();
            for (key, entry) in map {
                path.push(WalkSegment::Key(key.clone()));
                // The reserved comment namespace cuts both ways: a
                // `//`-prefixed key written here would be stripped as a
                // comment at the next parse — refuse loudly instead of
                // producing text this same adapter silently discards.
                if key.starts_with(COMMENT_PREFIX) {
                    return Err(FormatError::Serialize {
                        format: FORMAT,
                        message: reserved_key_message(&walk_label(path)),
                    });
                }
                let converted = value_to_json(entry, path)?;
                path.pop();
                out.insert(key.clone(), converted);
            }
            Json::Object(out)
        }
    })
}

// --- edit ----------------------------------------------------------------

/// Extract the key segments of a [`ConfigPath`], refusing `//`-prefixed
/// segments, which live in the reserved comment namespace and can never
/// address a configuration key.
fn key_segments(path: &ConfigPath) -> Result<Vec<&str>, FormatError> {
    path.segments()
        .iter()
        .map(|PathSegment::Key(k)| {
            if k.starts_with(COMMENT_PREFIX) {
                Err(FormatError::Edit {
                    format: FORMAT,
                    message: reserved_key_message(&format!("'{k}'")),
                })
            } else {
                Ok(k.as_str())
            }
        })
        .collect()
}

/// The JSON document tree behind the shared edit walkers (`format::edit`)
/// — the same conflict contract as the TOML adapter (a pre-existing
/// on-disk shape the schema never saw refuses typed).
impl super::edit::EditDoc for Json {
    type Value = Json;

    const FORMAT: &'static str = FORMAT;
    const CONTAINER: &'static str = "object";
    const CONTAINER_WITH_ARTICLE: &'static str = "an object";
    const SOURCE: &'static str = "document";

    fn is_container(&self) -> bool {
        self.is_object()
    }

    fn has_child(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    fn child_mut(&mut self, key: &str) -> Option<&mut Self> {
        self.get_mut(key)
    }

    fn insert_container(&mut self, key: &str) {
        self.as_object_mut()
            .expect("callers guarantee a container")
            .insert(key.to_string(), Json::Object(JsonMap::new()));
    }

    fn insert_value(&mut self, key: &str, value: Json) {
        let obj = self.as_object_mut().expect("callers guarantee a container");
        insert_adjacent_to_comment(obj, key, value);
    }

    fn remove_key(&mut self, key: &str) -> bool {
        self.as_object_mut()
            .is_some_and(|obj| obj.remove(key).is_some())
    }
}

/// Insert `leaf` into `obj`, keeping comment keys adjacent to the fields
/// they document: replacing an existing member keeps its position
/// (`preserve_order`), and a NEW member whose `"//leaf"` comment already
/// exists (a generated template documents defaultless fields this way)
/// lands immediately after that comment instead of at the end of the
/// object. Without a comment, a new member appends.
fn insert_adjacent_to_comment(obj: &mut JsonMap<String, Json>, leaf: &str, value: Json) {
    let comment = comment_key(leaf);
    if !obj.contains_key(leaf) && obj.contains_key(&comment) {
        let mut pending = Some(value);
        for (key, entry) in std::mem::take(obj) {
            let is_comment = key == comment;
            obj.insert(key, entry);
            if is_comment && let Some(v) = pending.take() {
                obj.insert(leaf.to_string(), v);
            }
        }
    } else {
        obj.insert(leaf.to_string(), value);
    }
}

// --- template emission ---------------------------------------------------

/// The JSON template renderer: each level is an object carrying a `"//"`
/// comment for its own prose and `"//field-name"` comments per field.
/// Unlike the text-format emitters, leaves and nested objects are NOT
/// reordered — JSON has no section-header rule forcing leaves first.
struct JsonTemplate;

impl TemplateRenderer for JsonTemplate {
    type Ctx = ();
    type Out = JsonMap<String, Json>;

    const LEAVES_FIRST: bool = false;

    /// A `//`-prefixed schema field name would render as a comment key and
    /// vanish at the next parse — refuse it here, where the name first
    /// meets JSON text.
    fn check_field_name(&self, name: &str) -> Result<(), FormatError> {
        if name.starts_with(COMMENT_PREFIX) {
            return Err(FormatError::Serialize {
                format: FORMAT,
                message: reserved_key_message(&format!("'{name}'")),
            });
        }
        Ok(())
    }

    /// The level's prose lands in its own object's `"//"` slot, not as a
    /// sibling `"//name"` comment — one home per comment.
    fn level_doc(&mut self, out: &mut Self::Out, doc: &[String]) {
        if !doc.is_empty() {
            out.insert(COMMENT_PREFIX.into(), comment_value(doc_lines(doc)));
        }
    }

    fn leaf(
        &mut self,
        out: &mut Self::Out,
        _ctx: &(),
        name: &str,
        leaf: &Leaf,
    ) -> Result<(), FormatError> {
        let mut lines = leaf_annotations(leaf, "JSON", &mut |v| inline_json(v, name))?;
        match &leaf.default {
            Some(default) => {
                if !lines.is_empty() {
                    out.insert(comment_key(name), comment_value(lines));
                }
                let mut path = vec![WalkSegment::Key(name.to_string())];
                out.insert(name.to_string(), value_to_json(default, &mut path)?);
            }
            None => {
                // JSON cannot comment out a real key, so the assignment
                // snippet rides inside the comment — the counterpart of
                // TOML's `#name = ""` line.
                lines.push(assignment_snippet(
                    name,
                    placeholder_json(&leaf.ty).to_string(),
                ));
                out.insert(comment_key(name), comment_value(lines));
            }
        }
        Ok(())
    }

    fn nested(
        &mut self,
        out: &mut Self::Out,
        ctx: &(),
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        let mut obj = JsonMap::new();
        walk_level(self, child, ctx, &mut obj)?;
        out.insert(name.to_string(), Json::Object(obj));
        Ok(())
    }

    fn array_of(
        &mut self,
        out: &mut Self::Out,
        _ctx: &(),
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        // Entry count is the user's call, so no real key is emitted (an
        // absent array-of resolves to the empty list); the comment carries
        // a one-entry example.
        let mut lines = doc_lines(&child.doc);
        let example = Json::Array(vec![Json::Object(example_object(child, name)?)]);
        lines.push(assignment_snippet(name, compact(&example)));
        out.insert(comment_key(name), comment_value(lines));
        Ok(())
    }

    fn map_of(
        &mut self,
        out: &mut Self::Out,
        _ctx: &(),
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        // Entry keys are user-supplied, so no real key is emitted (an
        // absent map-of resolves to the empty map); the comment carries a
        // placeholder-keyed example.
        let mut lines = doc_lines(&child.doc);
        let mut example = JsonMap::new();
        example.insert(
            "<key>".to_string(),
            Json::Object(example_object(child, name)?),
        );
        lines.push(assignment_snippet(name, compact(&Json::Object(example))));
        out.insert(comment_key(name), comment_value(lines));
        Ok(())
    }
}

/// Example object for an array-of / map-of entry, shown inside a comment:
/// defaults where declared, placeholders elsewhere. `context_key` names
/// the field in conversion errors (non-finite float defaults).
fn example_object(
    schema: &Schema,
    context_key: &str,
) -> Result<JsonMap<String, Json>, FormatError> {
    let mut obj = JsonMap::new();
    for nf in &schema.fields {
        // Same reserved-namespace refusal as `template_object`: an example
        // entry's `//`-prefixed field would read as a comment.
        if nf.name.starts_with(COMMENT_PREFIX) {
            return Err(FormatError::Serialize {
                format: FORMAT,
                message: reserved_key_message(&format!("'{}'", nf.name)),
            });
        }
        let value = match &nf.field {
            Field::Leaf(leaf) => match &leaf.default {
                Some(default) => {
                    let mut path = vec![WalkSegment::Key(context_key.to_string())];
                    value_to_json(default, &mut path)?
                }
                None => placeholder_value(&leaf.ty),
            },
            Field::Nested(child) => Json::Object(example_object(child, context_key)?),
            Field::ArrayOf(child) => {
                Json::Array(vec![Json::Object(example_object(child, context_key)?)])
            }
            Field::MapOf(child) => {
                let mut entry = JsonMap::new();
                entry.insert(
                    "<key>".to_string(),
                    Json::Object(example_object(child, context_key)?),
                );
                Json::Object(entry)
            }
        };
        obj.insert(nf.name.clone(), value);
    }
    Ok(obj)
}

/// The comment key documenting `field_name` (`"//port"`).
fn comment_key(field_name: &str) -> String {
    format!("{COMMENT_PREFIX}{field_name}")
}

/// Comment payload per the convention: a single line is a plain string,
/// multi-line prose is an array of strings.
fn comment_value(lines: Vec<String>) -> Json {
    debug_assert!(!lines.is_empty(), "callers only emit non-empty comments");
    if lines.len() == 1 {
        Json::String(lines.into_iter().next().expect("length checked"))
    } else {
        Json::Array(lines.into_iter().map(Json::String).collect())
    }
}

/// The `"key": value` snippet a defaultless field's comment shows — what
/// the user pastes (uncommented) to set the field.
fn assignment_snippet(field_name: &str, value_json: String) -> String {
    format!(
        "{}: {value_json}",
        compact(&Json::String(field_name.to_string()))
    )
}

/// Compact single-line JSON rendering, for snippets inside comments.
fn compact(json: &Json) -> String {
    serde_json::to_string(json).expect("serde_json::Value serialization is infallible")
}

/// Render one owned value as inline JSON (for `Allowed:` enum listings),
/// naming `key` in conversion errors.
fn inline_json(value: &Value, key: &str) -> Result<String, FormatError> {
    let mut path = vec![WalkSegment::Key(key.to_string())];
    Ok(compact(&value_to_json(value, &mut path)?))
}

/// Placeholder rendered in an assignment snippet for a leaf without a
/// default, hinting the expected value shape: the shared table with JSON's
/// quoted spellings for the string and datetime arms.
fn placeholder_json(ty: &LeafType) -> &'static str {
    super::template::placeholder(ty, "\"\"", "\"1970-01-01T00:00:00Z\"")
}

/// [`placeholder_json`] as a `serde_json::Value`, for example objects.
fn placeholder_value(ty: &LeafType) -> Json {
    serde_json::from_str(placeholder_json(ty)).expect("placeholders are valid JSON")
}

#[cfg(test)]
mod tests {
    use super::super::SetTarget;
    use super::*;

    // --- capabilities ----------------------------------------------------

    #[test]
    fn json_adapter_declares_its_matrix_rows() {
        // The ADR-0002 matrix has no refusal rows for JSON, so the adapter
        // declares every implemented operation. SpanIndex stays undeclared
        // (and refuses typed) until the provenance epic builds the index.
        for operation in [
            Operation::Parse,
            Operation::Template,
            Operation::Serialize,
            Operation::EditSet,
            Operation::EditCreateKey,
            Operation::EditCreateFile,
            Operation::EditUnset,
        ] {
            assert!(
                JsonAdapter.supports(operation),
                "json should declare {operation}"
            );
        }
        assert!(!JsonAdapter.supports(Operation::SpanIndex));
        assert!(matches!(
            JsonAdapter.span_index("{}").unwrap_err(),
            FormatError::Unsupported(_)
        ));
    }

    // --- parse: direct rows ----------------------------------------------

    #[test]
    fn parse_scalars_and_containers() {
        let value = JsonAdapter
            .parse(r#"{"s": "x", "i": 3, "f": 1.5, "b": true, "t": {"n": 1, "arr": [1, 2]}}"#)
            .unwrap();
        let map = value.as_map().unwrap();
        assert_eq!(map["s"], Value::String("x".into()));
        assert_eq!(map["i"], Value::Integer(3));
        assert_eq!(map["f"], Value::Float(1.5));
        assert_eq!(map["b"], Value::Boolean(true));
        let t = map["t"].as_map().unwrap();
        assert_eq!(t["n"], Value::Integer(1));
        assert_eq!(
            t["arr"],
            Value::Array(vec![Value::Integer(1), Value::Integer(2)])
        );
    }

    #[test]
    fn parse_number_forms() {
        let value = JsonAdapter
            .parse(r#"{"exp": 1e3, "neg": -7, "max": 9223372036854775807}"#)
            .unwrap();
        let map = value.as_map().unwrap();
        // An exponent literal is a float, an integer literal an integer.
        assert_eq!(map["exp"], Value::Float(1000.0));
        assert_eq!(map["neg"], Value::Integer(-7));
        assert_eq!(map["max"], Value::Integer(i64::MAX));
    }

    #[test]
    fn parse_whitespace_only_is_empty_map() {
        assert_eq!(JsonAdapter.parse("").unwrap(), Value::Map(Map::new()));
        assert_eq!(JsonAdapter.parse("  \n\t").unwrap(), Value::Map(Map::new()));
    }

    #[test]
    fn parse_non_object_root_maps_shape_faithfully() {
        // Root-shape policy lives in the pipeline (`resolve` rejects
        // non-map roots with one shared error); the adapter maps what the
        // text says.
        assert_eq!(
            JsonAdapter.parse("[1, 2]").unwrap(),
            Value::Array(vec![Value::Integer(1), Value::Integer(2)])
        );
    }

    // --- parse: comment stripping (reserved `//` namespace) ---------------

    #[test]
    fn parse_strips_comment_keys_at_every_depth() {
        let value = JsonAdapter
            .parse(
                r#"{
                    "//": ["top-level prose"],
                    "//host": "docs for host",
                    "host": "localhost",
                    "db": {
                        "//": "section prose",
                        "//url": "docs for url",
                        "url": "pg://x"
                    },
                    "servers": [{"//name": "docs in an array element", "name": "a"}]
                }"#,
            )
            .unwrap();
        let map = value.as_map().unwrap();
        assert_eq!(
            map.keys().map(String::as_str).collect::<Vec<_>>(),
            ["db", "host", "servers"],
            "no //-prefixed member reaches the tree"
        );
        let db = map["db"].as_map().unwrap();
        assert_eq!(db.keys().map(String::as_str).collect::<Vec<_>>(), ["url"]);
        let server = map["servers"].as_array().unwrap()[0].as_map().unwrap();
        assert_eq!(
            server.keys().map(String::as_str).collect::<Vec<_>>(),
            ["name"]
        );
    }

    #[test]
    fn parse_strips_comment_members_whatever_their_value_shape() {
        // The whole member is comment syntax — even a null or object
        // comment value must not trip the mapping-table errors.
        let value = JsonAdapter
            .parse(r#"{"//weird": null, "//huge": 18446744073709551615, "ok": 1}"#)
            .unwrap();
        assert_eq!(
            value
                .as_map()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["ok"]
        );
    }

    // --- parse: mapping-table error rows ----------------------------------

    #[test]
    fn parse_null_is_typed_error_naming_key_and_advising_absence() {
        let err = JsonAdapter.parse(r#"{"db": {"url": null}}"#).unwrap_err();
        match err {
            FormatError::Parse {
                format, message, ..
            } => {
                assert_eq!(format, "json");
                assert!(message.contains("'db.url'"), "names the key: {message}");
                assert!(
                    message.contains("absence expresses unset"),
                    "advises absence: {message}"
                );
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_null_in_array_names_indexed_path() {
        let err = JsonAdapter.parse(r#"{"xs": [1, null]}"#).unwrap_err();
        assert!(
            err.detail().contains("'xs[1]'"),
            "names the element: {}",
            err.detail()
        );
    }

    #[test]
    fn parse_integer_outside_i64_is_typed_error_naming_key() {
        // i64::MAX + 1 — lexically an integer, outside the value model.
        let err = JsonAdapter
            .parse(r#"{"big": 9223372036854775808}"#)
            .unwrap_err();
        match err {
            FormatError::Parse { message, .. } => {
                assert!(message.contains("'big'"), "names the key: {message}");
                assert!(message.contains("out of range"), "{message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_below_i64_is_typed_error_naming_key() {
        // i64::MIN - 1 — the negative side of the range check. Without the
        // lexical form this would silently coerce to a float.
        let err = JsonAdapter
            .parse(r#"{"low": -9223372036854775809}"#)
            .unwrap_err();
        match err {
            FormatError::Parse { message, .. } => {
                assert!(message.contains("'low'"), "names the key: {message}");
                assert!(message.contains("out of range"), "{message}");
                assert!(message.contains("-9223372036854775809"), "{message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_above_u64_is_typed_error_naming_key() {
        // u64::MAX + 1 — beyond even serde_json's u64 fallback, still an
        // integer literal, still a typed error.
        let err = JsonAdapter
            .parse(r#"{"big": 18446744073709551616}"#)
            .unwrap_err();
        assert!(err.detail().contains("'big'"), "{}", err.detail());
        assert!(err.detail().contains("out of range"), "{}", err.detail());
    }

    #[test]
    fn parse_float_overflowing_f64_is_typed_error() {
        // A float literal too large for f64 must not become infinity.
        let err = JsonAdapter.parse(r#"{"huge": 1e999}"#).unwrap_err();
        assert!(err.detail().contains("'huge'"), "{}", err.detail());
        assert!(err.detail().contains("out of range"), "{}", err.detail());
    }

    #[test]
    fn edit_preserves_out_of_range_literals_in_untouched_keys() {
        // The edit path works on raw JSON text; a number the value model
        // rejects at parse must survive an unrelated edit verbatim (the
        // load gate still refuses the document) — never re-rendered as a
        // lossy float.
        let source = r#"{"big": 18446744073709551616, "low": -9223372036854775809, "port": 1}"#;
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(2);
        let out = JsonAdapter
            .edit(
                source,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::ExistingValue,
                },
            )
            .unwrap();
        assert!(out.contains("18446744073709551616"), "{out}");
        assert!(out.contains("-9223372036854775809"), "{out}");
        assert!(out.contains(r#""port": 2"#), "{out}");
    }

    #[test]
    fn parse_syntax_error_carries_message_and_span() {
        let source = "{\"key\": }";
        let err = JsonAdapter.parse(source).unwrap_err();
        match err {
            FormatError::Parse {
                format,
                message,
                span,
            } => {
                assert_eq!(format, "json");
                assert!(!message.is_empty());
                let span = span.expect("json syntax errors report a span");
                assert!(span.start < source.len());
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_syntax_error_span_is_byte_accurate_after_multibyte_chars() {
        // serde_json's column is a byte offset; with a multi-byte char
        // earlier on the line the span must still land on the offending
        // token, on valid char boundaries (slicing must not panic).
        let source = "{\"héllo\": }";
        let err = JsonAdapter.parse(source).unwrap_err();
        let FormatError::Parse { span, .. } = err else {
            panic!("expected Parse");
        };
        let span = span.expect("syntax errors carry a span");
        assert_eq!(&source[span.start..span.end], "}", "span: {span:?}");
    }

    // --- serialize --------------------------------------------------------

    #[test]
    fn serialize_round_trips_parse() {
        let source = r#"{"b": true, "i": 3, "s": "x", "t": {"n": 1}}"#;
        let value = JsonAdapter.parse(source).unwrap();
        let text = JsonAdapter.serialize(&value).unwrap();
        assert!(text.ends_with('\n'));
        let reparsed = JsonAdapter.parse(&text).unwrap();
        assert_eq!(value, reparsed);
    }

    #[test]
    fn serialize_datetime_as_toml_lexical_string() {
        let mut map = Map::new();
        let dt: crate::value::Datetime = "1979-05-27T07:32:00Z".parse().unwrap();
        map.insert("stamp".into(), Value::Datetime(dt));
        let text = JsonAdapter.serialize(&Value::Map(map)).unwrap();
        assert!(
            text.contains(r#""stamp": "1979-05-27T07:32:00Z""#),
            "{text}"
        );
    }

    #[test]
    fn serialize_hand_constructed_invalid_datetime_never_panics() {
        // `Datetime`'s component fields are public (`toml_datetime`'s
        // types); a hand-assembled non-grammatical value serializes as
        // its `Display` string — garbage in, garbage out, never a panic.
        use crate::value::{Date, Datetime};
        let mut map = Map::new();
        map.insert(
            "stamp".into(),
            Value::Datetime(Datetime {
                date: Some(Date {
                    year: 1979,
                    month: 13,
                    day: 1,
                }),
                time: None,
                offset: None,
            }),
        );
        let text = JsonAdapter.serialize(&Value::Map(map)).unwrap();
        assert!(text.contains(r#""stamp": "1979-13-01""#), "{text}");
    }

    #[test]
    fn serialize_offset_upstream_display_cannot_format_never_panics() {
        // `Offset::Custom { minutes: i16::MIN }` overflows upstream
        // `Display` (a panic in overflow-checked builds); the adapter
        // spells it through the value model's panic-free formatter:
        // garbage in, garbage out — 32768 minutes = 546h 08m.
        use crate::value::{Date, Datetime, Offset, Time};
        let mut map = Map::new();
        map.insert(
            "stamp".into(),
            Value::Datetime(Datetime {
                date: Some(Date {
                    year: 1979,
                    month: 5,
                    day: 27,
                }),
                time: Some(Time {
                    hour: 7,
                    minute: 32,
                    second: 0,
                    nanosecond: 0,
                }),
                offset: Some(Offset::Custom { minutes: i16::MIN }),
            }),
        );
        let text = JsonAdapter.serialize(&Value::Map(map)).unwrap();
        assert!(
            text.contains(r#""stamp": "1979-05-27T07:32:00-546:08""#),
            "{text}"
        );
    }

    #[test]
    fn serialize_non_finite_float_is_typed_error_naming_path() {
        let mut inner = Map::new();
        inner.insert("rate".into(), Value::Float(f64::INFINITY));
        let mut map = Map::new();
        map.insert("limits".into(), Value::Map(inner));
        let err = JsonAdapter.serialize(&Value::Map(map)).unwrap_err();
        match err {
            FormatError::Serialize { format, message } => {
                assert_eq!(format, "json");
                assert!(message.contains("non-finite"), "{message}");
                assert!(
                    message.contains("'limits.rate'"),
                    "names the path: {message}"
                );
            }
            other => panic!("expected Serialize, got {other:?}"),
        }

        let mut map = Map::new();
        map.insert("xs".into(), Value::Array(vec![Value::Float(f64::NAN)]));
        let err = JsonAdapter.serialize(&Value::Map(map)).unwrap_err();
        assert!(err.detail().contains("'xs[0]'"), "{}", err.detail());
    }

    #[test]
    fn serialize_reserved_comment_key_is_typed_error_naming_path() {
        // Parse strips every //-prefixed member, so writing one would
        // produce data this same adapter silently deletes on the next
        // read — the round trip must refuse instead.
        let mut inner = Map::new();
        inner.insert("//secret".into(), Value::String("x".into()));
        let mut map = Map::new();
        map.insert("db".into(), Value::Map(inner));
        let err = JsonAdapter.serialize(&Value::Map(map)).unwrap_err();
        match err {
            FormatError::Serialize { format, message } => {
                assert_eq!(format, "json");
                // ConfigPath quotes non-bareword segments in its dotted
                // rendering.
                assert!(
                    message.contains(r#"'db."//secret"'"#),
                    "names the path: {message}"
                );
                assert!(message.contains("reserved"), "{message}");
            }
            other => panic!("expected Serialize, got {other:?}"),
        }
    }

    #[test]
    fn edit_set_reserved_comment_path_is_typed_error() {
        // The same refusal at the edit boundary: a //-prefixed path
        // segment can never address a configuration key.
        let path = ConfigPath::new().key("//notes");
        let value = Value::from("x");
        let err = JsonAdapter
            .edit(
                "{}",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap_err();
        match err {
            FormatError::Edit { message, .. } => {
                assert!(message.contains("reserved"), "{message}");
                assert!(message.contains("//notes"), "{message}");
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn template_reserved_comment_field_name_is_typed_error() {
        // A runtime schema may carry any field name; one in the reserved
        // //-namespace would render as a comment and vanish at the next
        // parse, so template emission refuses it.
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field("//weird", Field::string().default("x"))
            .build();
        let err = JsonAdapter.template(&schema).unwrap_err();
        assert!(err.detail().contains("reserved"), "{}", err.detail());
        assert!(err.detail().contains("//weird"), "{}", err.detail());
    }

    // --- template ---------------------------------------------------------

    #[test]
    fn template_matches_byte_identical_golden() {
        // The JSON counterpart of the TOML adapter's golden: same demo
        // schema, documentation riding the "//" comment-key convention.
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .doc("Demo runtime schema")
            .field("host", Field::string().doc("App host").default("localhost"))
            .field("port", Field::integer().doc("Port number").default(8080i64))
            .field(
                "level",
                Field::enum_of(["debug", "info"])
                    .doc("Log verbosity")
                    .default("info"),
            )
            .field("name", Field::string().doc("Required name."))
            .field("rule", Field::value().doc("Any value."))
            .nested(
                "db",
                RtSchema::object("Db")
                    .doc("Database settings")
                    .field("url", Field::string().optional())
                    .field("pool_size", Field::integer().default(5i64)),
            )
            .build();
        let golden = r#"{
  "//": "Demo runtime schema",
  "//host": "App host",
  "host": "localhost",
  "//port": "Port number",
  "port": 8080,
  "//level": [
    "Log verbosity",
    "Allowed: \"debug\" | \"info\""
  ],
  "level": "info",
  "//name": [
    "Required name.",
    "\"name\": \"\""
  ],
  "//rule": [
    "Any value.",
    "Accepts: any JSON value",
    "\"rule\": \"\""
  ],
  "db": {
    "//": "Database settings",
    "//url": "\"url\": \"\"",
    "pool_size": 5
  }
}
"#;
        assert_eq!(JsonAdapter.template(&schema).unwrap(), golden);
    }

    #[test]
    fn template_array_of_and_map_of_ride_comments_only() {
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .array_of(
                "plugins",
                RtSchema::object("Plugin")
                    .doc("Plugin entries.")
                    .field("name", Field::string())
                    .field("enabled", Field::boolean().default(true)),
            )
            .map_of(
                "servers",
                RtSchema::object("Server").field("host", Field::string().default("localhost")),
            )
            .build();
        let text = JsonAdapter.template(&schema).unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let obj = json.as_object().unwrap();
        // No real keys — absent array-of/map-of resolve to empty; the
        // examples live in the comments.
        assert!(!obj.contains_key("plugins"));
        assert!(!obj.contains_key("servers"));
        let plugins = obj["//plugins"].as_array().unwrap();
        assert_eq!(plugins[0], "Plugin entries.");
        assert_eq!(plugins[1], r#""plugins": [{"name":"","enabled":true}]"#);
        assert_eq!(
            obj["//servers"],
            r#""servers": {"<key>":{"host":"localhost"}}"#
        );
    }

    #[test]
    fn template_parses_clean_through_own_adapter() {
        // gen → parse: every comment key is stripped, every real key is a
        // schema key with its default value.
        use crate::fixtures::test::test_schema;
        let text = JsonAdapter.template(&test_schema()).unwrap();
        let value = JsonAdapter.parse(&text).unwrap();
        let map = value.as_map().unwrap();
        assert_eq!(
            map.keys().map(String::as_str).collect::<Vec<_>>(),
            ["database", "debug", "host", "port"]
        );
        assert_eq!(map["host"], Value::String("localhost".into()));
        let db = map["database"].as_map().unwrap();
        assert_eq!(
            db.keys().map(String::as_str).collect::<Vec<_>>(),
            ["pool_size"],
            "optional defaultless url stays absent"
        );
    }

    // --- edit ---------------------------------------------------------------

    #[test]
    fn edit_set_preserves_comments_and_their_placement() {
        // Comments are data, so they survive the round trip; document key
        // order is preserved, so the comment stays adjacent to its field.
        let source = "{\n  \"//port\": \"my note\",\n  \"port\": 8080,\n  \"host\": \"x\"\n}\n";
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(3000);
        let out = JsonAdapter
            .edit(
                source,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::ExistingValue,
                },
            )
            .unwrap();
        let comment_at = out
            .find("\"//port\": \"my note\"")
            .expect("comment survives");
        let port_at = out.find("\"port\": 3000").expect("value replaced");
        let host_at = out.find("\"host\": \"x\"").expect("untouched key survives");
        assert!(
            comment_at < port_at && port_at < host_at,
            "document order is preserved: {out}"
        );
    }

    #[test]
    fn edit_round_trip_keeps_comments_as_data_and_strips_them_at_parse() {
        let source = r#"{"//": "file prose", "//port": "docs", "port": 1}"#;
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(2);
        let edited = JsonAdapter
            .edit(
                source,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::ExistingValue,
                },
            )
            .unwrap();
        // Edit a second time (unset a key that isn't there) — comments
        // keep riding along.
        let missing = ConfigPath::new().key("nope");
        let edited = JsonAdapter
            .edit(&edited, FileEdit::Unset { path: &missing })
            .unwrap();
        assert!(edited.contains(r#""//": "file prose""#));
        assert!(edited.contains(r#""//port": "docs""#));
        // And parse still owns the namespace: comments never reach the tree.
        let tree = JsonAdapter.parse(&edited).unwrap();
        let map = tree.as_map().unwrap();
        assert_eq!(map.keys().map(String::as_str).collect::<Vec<_>>(), ["port"]);
        assert_eq!(map["port"], Value::Integer(2));
    }

    #[test]
    fn edit_set_created_key_lands_adjacent_to_its_comment() {
        // A generated template documents a defaultless field as a
        // "//field" comment with no real key. Setting that field must
        // place the new member right after its comment — not at the end
        // of the object, where the documentation would be orphaned.
        let source = concat!(
            "{\n",
            "  \"//name\": [\"Required name.\", \"\\\"name\\\": \\\"\\\"\"],\n",
            "  \"host\": \"x\"\n",
            "}\n"
        );
        let path = ConfigPath::new().key("name");
        let value = Value::from("demo");
        let out = JsonAdapter
            .edit(
                source,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap();
        let comment_at = out.find("\"//name\"").expect("comment survives");
        let name_at = out.find("\"name\": \"demo\"").expect("value written");
        let host_at = out.find("\"host\": \"x\"").expect("untouched key survives");
        assert!(
            comment_at < name_at && name_at < host_at,
            "new key sits right after its comment: {out}"
        );

        // Unset and re-set: the field returns to its documented slot.
        let out = JsonAdapter
            .edit(&out, FileEdit::Unset { path: &path })
            .unwrap();
        let out = JsonAdapter
            .edit(
                &out,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap();
        let comment_at = out.find("\"//name\"").expect("comment survives");
        let name_at = out.find("\"name\": \"demo\"").expect("value re-written");
        let host_at = out.find("\"host\": \"x\"").expect("untouched key survives");
        assert!(
            comment_at < name_at && name_at < host_at,
            "re-set key returns to its documented slot: {out}"
        );
    }

    #[test]
    fn edit_set_creates_missing_path() {
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let out = JsonAdapter
            .edit(
                "",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap();
        let reparsed = JsonAdapter.parse(&out).unwrap();
        assert_eq!(
            reparsed.as_map().unwrap()["database"].as_map().unwrap()["url"],
            Value::String("pg://x".into())
        );
    }

    #[test]
    fn edit_set_path_conflict_is_typed_error() {
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let err = JsonAdapter
            .edit(
                r#"{"database": "oops"}"#,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap_err();
        match err {
            FormatError::Edit { message, .. } => assert!(message.contains("path conflict")),
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn edit_unset_removes_key_and_missing_is_noop() {
        let path = ConfigPath::new().key("port");
        let out = JsonAdapter
            .edit(
                r#"{"port": 1, "host": "x"}"#,
                FileEdit::Unset { path: &path },
            )
            .unwrap();
        assert!(!out.contains("port"));
        assert!(out.contains(r#""host": "x""#));

        let missing = ConfigPath::new().key("nope").key("deep");
        let unchanged = JsonAdapter
            .edit(r#"{"port": 1}"#, FileEdit::Unset { path: &missing })
            .unwrap();
        assert!(unchanged.contains(r#""port": 1"#));
    }

    #[test]
    fn edit_set_non_finite_float_is_typed_serialize_error() {
        let path = ConfigPath::new().key("rate");
        let value = Value::Float(f64::NAN);
        let err = JsonAdapter
            .edit(
                "{}",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap_err();
        assert!(matches!(err, FormatError::Serialize { .. }));
    }

    #[test]
    fn missing_file_seeding_starts_from_documented_template() {
        // The persist path's create-file row: no file content → the new
        // document is the generated template (comment keys included) with
        // the set applied.
        use crate::fixtures::test::test_schema;
        let out = crate::persist::set_in_document_runtime(
            &JsonAdapter,
            &test_schema(),
            None,
            "port",
            "9090",
        )
        .unwrap();
        assert!(
            out.contains(r#""//host": "The application host.""#),
            "{out}"
        );
        let tree = JsonAdapter.parse(&out).unwrap();
        assert_eq!(tree.as_map().unwrap()["port"], Value::Integer(9090));
    }
}
