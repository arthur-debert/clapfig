//! YAML format adapter — `serde_norway` parsing, `yamlpath`/`yamlpatch`
//! editing (ADR-0003), `yamlpath` span index inside the same parse
//! (ADR-0008).
//!
//! This file is the ONLY place in the crate (outside `Cargo.toml`) that
//! touches the YAML crates: `serde_norway` (parse/serialize; the
//! `yaml_serde` sibling exists solely because `yamlpatch`'s patch values
//! are its type) and `yamlpath` + `yamlpatch` (span index + targeted
//! span-level edits).
//!
//! Baseline mapping (ADR-0002's table, YAML rows):
//!
//! - **Strict scalars** — only `true`/`false` spellings are booleans; `no`,
//!   `yes`, `on`, `off` parse as strings (no Norway problem; locked by
//!   tests).
//! - **Aliases** resolve at parse, invisible to the model; **custom tags**
//!   and **merge keys** (`<<`) are typed errors naming the offending key.
//!   A path that exists in source gets yamlpath's key and value ranges; a
//!   path that exists in [`Value`] only because an alias expanded gets
//!   the `*name` token's span for both `key` and `value` (ADR-0008).
//!   yamlpath can follow aliases internally — this adapter does not use
//!   that, because those spans sit on the anchor, not the alias site.
//! - **`null`/`~`** is a typed error advising absence; an empty or
//!   comments-only document — bare `---`/`...` document markers included —
//!   is the empty map (absence, not null).
//! - **Non-string mapping keys** are typed errors.
//! - **Integers** outside `i64` are typed errors; **`.inf`/`.nan`** parse
//!   into the model's non-finite floats.
//! - **Datetimes** arrive as strings in TOML's four lexical forms; the
//!   schema-driven coercion pass (ADR-0001) turns them into
//!   [`Value::Datetime`] — this adapter never guesses.
//!
//! Editing is span surgery: `yamlpatch` patches the target's bytes and is
//! byte-preserving outside the edited span. `yamlpatch` is scoped to
//! targeted single-value patches (ADR-0003), so every edit is verified
//! after patching — the result must reparse to exactly the intended tree —
//! and any shape the stack cannot patch honestly (sequence items, flow
//! collection members whose line the patcher would mangle) surfaces as the
//! typed [`UnsupportedByFormat`](super::UnsupportedByFormat) refusal
//! instead of silent corruption.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::runtime::{Field, Leaf, Schema};
use crate::value::{Map, Value};

use super::template::{
    TemplateRenderer, leaf_annotations, placeholder, push_comment_line, push_commented_block,
    walk_level,
};
use super::{
    ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, Parsed, PathSegment, Span,
    SpanEntry, UnsupportedByFormat, walk_label,
};

/// The YAML format behind the adapter contract.
///
/// Declares every ADR-0002 matrix row; known refusals are shape-level,
/// inside the declared edits — sequence-item edits and flow-style shapes
/// the patch stack cannot rewrite honestly. Span indexing rides on
/// [`parse`](YamlAdapter::parse) (ADR-0005): the same call fills
/// [`Parsed::spans`] via `yamlpath` (ADR-0008). See the [module docs](self).
pub struct YamlAdapter;

impl FormatAdapter for YamlAdapter {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn capabilities(&self) -> &'static [Operation] {
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

    fn display_entry(&self, key: &str, value: &str) -> String {
        // One-line YAML scalar: a plain dotted path stays bare, but
        // runtime-schema names YAML cannot carry as a plain scalar
        // (embedded `: `, `#`, quotes, leading indicators, control
        // characters) come out quoted. Control characters take JSON
        // string encoding — valid YAML — because serde_norway would
        // emit a block scalar (`|-\n  a\n  b`) and appending `: {value}`
        // would put the assignment on the block's last content line.
        format!("{}: {value}", inline_scalar(key))
    }

    fn parse(&self, text: &str) -> Result<Parsed, FormatError> {
        // An empty or comments-only file (bare document markers included)
        // is an empty config: absence, not null. serde_norway would read
        // these as a root Null (or reject a lone `...`); only a document
        // with actual non-comment content gets the null error below.
        if is_blank_or_comments(text) {
            return Ok(Parsed::from_value(Value::Map(Map::new())));
        }
        let raw: serde_norway::Value =
            serde_norway::from_str(text).map_err(|e| parse_error(&e, text))?;
        let value = norway_to_value(raw, &mut Vec::new())?;
        let spans = yaml_span_index(text, &value);
        Ok(Parsed { value, spans })
    }

    fn serialize(&self, value: &Value) -> Result<String, FormatError> {
        serde_norway::to_string(&value_to_norway(value)).map_err(|e| FormatError::Serialize {
            format: "yaml",
            message: e.to_string(),
        })
    }

    fn template(&self, schema: &Schema) -> Result<String, FormatError> {
        let mut out = String::new();
        for line in &schema.doc {
            push_comment_line(&mut out, "", line);
        }
        if !schema.doc.is_empty() {
            out.push('\n');
        }
        walk_level(&mut YamlTemplate, schema, &0, &mut out)?;
        Ok(out)
    }

    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        let operation = edit.operation();
        match edit {
            FileEdit::Set { path, value, .. } => {
                let keys = key_segments(path)?;
                set_in_source(source, &keys, value, operation)
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path)?;
                unset_in_source(source, &keys)
            }
        }
    }
}

/// `true` when every line is blank, a `#` comment, or a bare `---`/`...`
/// document marker — the "no content" document that parses to the empty
/// map instead of the null error (`serde_norway` reads a lone `---` as a
/// null document and rejects a lone `...` outright).
fn is_blank_or_comments(text: &str) -> bool {
    text.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..."
    })
}

/// Byte offset of the line carrying the first `...` end-of-document
/// marker, if any. Content inserted into a blank-or-comments source must
/// land before this line: text after `...` sits outside the document.
fn end_marker_offset(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim() == "..." {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Map a `serde_norway` parse failure into the shared error, carrying the
/// reported location as a single-character span when present. Both ends
/// are held to char boundaries — the reported byte offset is snapped back
/// to the boundary it sits past, and the span steps to the next boundary,
/// never one raw byte — because a span sliced mid-way through a multibyte
/// character would panic downstream error reporters.
fn parse_error(e: &serde_norway::Error, text: &str) -> FormatError {
    FormatError::Parse {
        format: "yaml",
        message: e.to_string(),
        span: e.location().map(|l| {
            let mut start = l.index().min(text.len());
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let end = text[start..]
                .chars()
                .next()
                .map_or(start, |c| start + c.len_utf8());
            Span { start, end }
        }),
    }
}

fn mapping_error(message: String) -> FormatError {
    FormatError::Parse {
        format: "yaml",
        message,
        span: None,
    }
}

/// Convert a parsed `serde_norway::Value` into the owned model, applying
/// ADR-0002's YAML mapping rows. `path` names the offending key in every
/// error.
fn norway_to_value(
    value: serde_norway::Value,
    path: &mut Vec<PathSegment>,
) -> Result<Value, FormatError> {
    match value {
        serde_norway::Value::Null => Err(mapping_error(format!(
            "null at {}: absence expresses unset; null is not a configuration value — omit the key instead",
            walk_label(path)
        ))),
        serde_norway::Value::Bool(b) => Ok(Value::Boolean(b)),
        serde_norway::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if n.is_u64() {
                Err(mapping_error(format!(
                    "integer {n} at {} is out of range: integers are 64-bit signed",
                    walk_label(path)
                )))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Float(f))
            } else {
                // Unreachable with serde_norway's i64/u64/f64 number repr,
                // but untrusted input earns a typed error, never a panic.
                Err(mapping_error(format!(
                    "number {n} at {} is outside the supported numeric range: expected a 64-bit integer or float",
                    walk_label(path)
                )))
            }
        }
        serde_norway::Value::String(s) => Ok(Value::String(s)),
        serde_norway::Value::Sequence(items) => {
            let mut array = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                path.push(PathSegment::Index(i));
                let converted = norway_to_value(item, path)?;
                path.pop();
                array.push(converted);
            }
            Ok(Value::Array(array))
        }
        serde_norway::Value::Mapping(entries) => {
            let mut map = Map::new();
            for (key, entry) in entries {
                let serde_norway::Value::String(key) = key else {
                    return Err(mapping_error(format!(
                        "non-string mapping key `{}` at {}: configuration keys are strings",
                        inline_norway(&key),
                        walk_label(path)
                    )));
                };
                if key == "<<" {
                    return Err(mapping_error(format!(
                        "YAML merge key '<<' at {} is outside the configuration baseline: spell the keys out",
                        walk_label(path)
                    )));
                }
                path.push(PathSegment::Key(key.clone()));
                let converted = norway_to_value(entry, path)?;
                path.pop();
                map.insert(key, converted);
            }
            Ok(Value::Map(map))
        }
        serde_norway::Value::Tagged(tagged) => Err(mapping_error(format!(
            "YAML tag '{}' at {} is outside the configuration baseline",
            tagged.tag,
            walk_label(path)
        ))),
    }
}

/// Best-effort inline rendering of a raw YAML value for error messages.
fn inline_norway(value: &serde_norway::Value) -> String {
    serde_norway::to_string(value)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_else(|_| format!("{value:?}"))
}

/// Fill a path → [`SpanEntry`] index for every node in `value` (ADR-0005,
/// ADR-0008). The root itself is not an entry — it has no key token and
/// unknown-key / value diagnostics never look it up — so an empty map
/// (including blank and comments-only documents) yields an empty index.
///
/// `yamlpath` is queried per written path. It can follow aliases; this
/// walk does not. When a node's exact span sits outside its pretty span
/// the value is an alias, and every descendant inherits the `*name`
/// token for both `key` and `value`.
fn yaml_span_index(text: &str, value: &Value) -> BTreeMap<ConfigPath, SpanEntry> {
    let mut spans = BTreeMap::new();
    match yamlpath::Document::new(text.to_string()) {
        Ok(doc) => fill_spans(&doc, value, &mut Vec::new(), None, &mut spans),
        // tree-sitter rejected something serde_norway accepted: still
        // cover every path so the index is complete, with a coarse
        // whole-document range rather than a silent hole.
        Err(_) => fill_fallback(value, &mut Vec::new(), whole_document(text), &mut spans),
    }
    spans
}

fn whole_document(text: &str) -> Span {
    Span {
        start: 0,
        end: text.len(),
    }
}

fn fill_spans(
    doc: &yamlpath::Document,
    value: &Value,
    path: &mut Vec<PathSegment>,
    inherited_alias: Option<Span>,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) {
    let child_alias = if path.is_empty() {
        None
    } else if let Some(alias) = inherited_alias {
        // ADR-0008: expanded nested paths caret the `*name` token for
        // both sides, including array elements that have no key in source.
        spans.insert(
            ConfigPath::from(path.clone()),
            SpanEntry {
                key: Some(alias),
                value: alias,
            },
        );
        Some(alias)
    } else {
        let (entry, alias) = locate_span_entry(doc, path);
        spans.insert(ConfigPath::from(path.clone()), entry);
        alias
    };

    match value {
        Value::Map(map) => {
            for (key, child) in map {
                path.push(PathSegment::Key(key.clone()));
                fill_spans(doc, child, path, child_alias, spans);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(PathSegment::Index(i));
                fill_spans(doc, child, path, child_alias, spans);
                path.pop();
            }
        }
        _ => {}
    }
}

fn fill_fallback(
    value: &Value,
    path: &mut Vec<PathSegment>,
    fallback: Span,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) {
    if !path.is_empty() {
        let key = match path.last() {
            Some(PathSegment::Key(_)) => Some(fallback),
            _ => None,
        };
        spans.insert(
            ConfigPath::from(path.clone()),
            SpanEntry {
                key,
                value: fallback,
            },
        );
    }
    match value {
        Value::Map(map) => {
            for (key, child) in map {
                path.push(PathSegment::Key(key.clone()));
                fill_fallback(child, path, fallback, spans);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(PathSegment::Index(i));
                fill_fallback(child, path, fallback, spans);
                path.pop();
            }
        }
        _ => {}
    }
}

fn locate_span_entry(doc: &yamlpath::Document, path: &[PathSegment]) -> (SpanEntry, Option<Span>) {
    let route = route_from_segments(path);
    let key = match path.last() {
        Some(PathSegment::Key(_)) => doc.query_key_only(&route).ok().map(|f| feature_span(&f)),
        _ => None,
    };
    let pretty = doc.query_pretty(&route).ok().map(|f| feature_span(&f));
    let exact = doc
        .query_exact(&route)
        .ok()
        .flatten()
        .map(|f| feature_span(&f));

    // yamlpath's exact mode resolves aliases, so a jump off the pretty
    // pair means this assignment is `*name`, not a written mapping.
    let alias = match (pretty, exact) {
        (Some(pretty), Some(exact)) if !span_contains(pretty, exact) => {
            extract_alias_token(doc.source(), pretty, key).or(Some(pretty))
        }
        _ => None,
    };
    if let Some(alias) = alias {
        return (SpanEntry { key, value: alias }, Some(alias));
    }
    let value = exact
        .or(pretty)
        .unwrap_or_else(|| whole_document(doc.source()));
    (SpanEntry { key, value }, None)
}

fn route_from_segments(segments: &[PathSegment]) -> yamlpath::Route<'static> {
    yamlpath::Route::from(
        segments
            .iter()
            .map(|segment| match segment {
                PathSegment::Key(key) => {
                    yamlpath::Component::Key(std::borrow::Cow::Owned(key.clone()))
                }
                PathSegment::Index(i) => yamlpath::Component::Index(*i),
            })
            .collect::<Vec<_>>(),
    )
}

fn feature_span(feature: &yamlpath::Feature<'_>) -> Span {
    let (start, end) = feature.location.byte_span;
    Span { start, end }
}

fn span_contains(outer: Span, inner: Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

/// The `*name` token inside a pretty pair (`db: *defaults`) or sequence
/// item (`*defaults`). Search starts after the key token so a key that
/// happens to contain `*` is not mistaken for the alias.
fn extract_alias_token(source: &str, pretty: Span, key: Option<Span>) -> Option<Span> {
    let start = key.map_or(pretty.start, |k| k.end.max(pretty.start));
    let end = pretty.end.min(source.len());
    if start >= end {
        return None;
    }
    let region = &source[start..end];
    let star = region.find('*')?;
    let token_start = start + star;
    let rest = &source[token_start + 1..end];
    let name_len = rest
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ']' | '}' | '#' | ':'))
        .unwrap_or(rest.len());
    if name_len == 0 {
        return None;
    }
    Some(Span {
        start: token_start,
        end: token_start + 1 + name_len,
    })
}

/// Convert an owned [`Value`] into a `serde_norway::Value` for
/// serialization. Datetimes serialize as strings in their lexical form —
/// the schema-driven coercion pass restores them on the way back in.
fn value_to_norway(value: &Value) -> serde_norway::Value {
    match value {
        Value::String(s) => serde_norway::Value::String(s.clone()),
        Value::Integer(i) => serde_norway::Value::Number((*i).into()),
        Value::Float(f) => serde_norway::Value::Number((*f).into()),
        Value::Boolean(b) => serde_norway::Value::Bool(*b),
        // The panic-free spelling, not upstream `Display` (see the
        // `value::datetime` module docs).
        Value::Datetime(d) => serde_norway::Value::String(crate::value::lexical_string(d)),
        Value::Array(items) => {
            serde_norway::Value::Sequence(items.iter().map(value_to_norway).collect())
        }
        Value::Map(map) => {
            let mut mapping = serde_norway::Mapping::new();
            for (k, v) in map {
                mapping.insert(serde_norway::Value::String(k.clone()), value_to_norway(v));
            }
            serde_norway::Value::Mapping(mapping)
        }
    }
}

// --- editing (yamlpath/yamlpatch span surgery) ---------------------------

/// The key segments of a [`ConfigPath`], for the edit path below. Index
/// segments refuse rather than silently retargeting the edit.
fn key_segments(path: &ConfigPath) -> Result<Vec<&str>, FormatError> {
    super::edit::map_key_segments(path, "yaml")
}

/// Convert an owned [`Value`] into a `yaml_serde::Value` — the type
/// `yamlpatch` patch operations carry. Same rules as [`value_to_norway`].
fn value_to_patch(value: &Value) -> yaml_serde::Value {
    match value {
        Value::String(s) => yaml_serde::Value::String(s.clone()),
        Value::Integer(i) => yaml_serde::Value::Number((*i).into()),
        Value::Float(f) => yaml_serde::Value::Number((*f).into()),
        Value::Boolean(b) => yaml_serde::Value::Bool(*b),
        Value::Datetime(d) => yaml_serde::Value::String(crate::value::lexical_string(d)),
        Value::Array(items) => {
            yaml_serde::Value::Sequence(items.iter().map(value_to_patch).collect())
        }
        Value::Map(map) => {
            let mut mapping = yaml_serde::Mapping::new();
            for (k, v) in map {
                mapping.insert(yaml_serde::Value::String(k.clone()), value_to_patch(v));
            }
            yaml_serde::Value::Mapping(mapping)
        }
    }
}

/// The tree a successful edit must reparse to: datetimes become their
/// string form (parse never produces [`Value::Datetime`]; the schema pass
/// owns that coercion).
fn as_parsed(value: &Value) -> Value {
    match value {
        Value::Datetime(d) => Value::String(crate::value::lexical_string(d)),
        Value::Array(items) => Value::Array(items.iter().map(as_parsed).collect()),
        Value::Map(map) => Value::Map(map.iter().map(|(k, v)| (k.clone(), as_parsed(v))).collect()),
        other => other.clone(),
    }
}

/// Tree equality with `NaN == NaN`, so verification of an edit that writes
/// a non-finite float does not refuse over IEEE 754 inequality.
///
/// Map comparison zips the two entry sequences: [`Map`] is a `BTreeMap`,
/// so iteration is key-sorted regardless of insertion order — equal
/// lengths plus pairwise-equal sorted entries is exactly map equality,
/// and a container re-emitted at the end of the file (remove + add)
/// still compares equal.
fn trees_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => (x.is_nan() && y.is_nan()) || x == y,
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| trees_equal(x, y))
        }
        (Value::Map(xs), Value::Map(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((ka, va), (kb, vb))| ka == kb && trees_equal(va, vb))
        }
        _ => a == b,
    }
}

/// The owned [`Value`] tree behind the shared edit walkers
/// (`format::edit`). The YAML adapter edits by patching the parsed tree
/// and verifying the patched source reparses to it, so its "document"
/// for the walkers is the owned model itself.
impl super::edit::EditDoc for Value {
    type Value = Value;

    const FORMAT: &'static str = "yaml";
    const CONTAINER: &'static str = "map";
    const CONTAINER_WITH_ARTICLE: &'static str = "a map";
    const SOURCE: &'static str = "file";

    fn is_container(&self) -> bool {
        matches!(self, Value::Map(_))
    }

    fn has_child(&self, key: &str) -> bool {
        self.as_map().is_some_and(|map| map.contains_key(key))
    }

    fn child_mut(&mut self, key: &str) -> Option<&mut Self> {
        self.as_map_mut()?.get_mut(key)
    }

    fn insert_container(&mut self, key: &str) {
        self.as_map_mut()
            .expect("callers guarantee a container")
            .insert(key.to_string(), Value::Map(Map::new()));
    }

    fn insert_value(&mut self, key: &str, value: Value) {
        self.as_map_mut()
            .expect("callers guarantee a container")
            .insert(key.to_string(), value);
    }

    fn remove_key(&mut self, key: &str) -> bool {
        self.as_map_mut()
            .is_some_and(|map| map.remove(key).is_some())
    }
}

/// Insert `value` at `keys` in `map`, creating intermediate maps. Errors on
/// a path conflict — an existing non-map value where the path needs a map.
/// A root adapter over the shared walker, which walks [`Value`] nodes (the
/// edited tree's root is a bare [`Map`]).
fn set_in_tree(map: &mut Map, keys: &[&str], value: Value) -> Result<(), FormatError> {
    let mut root = Value::Map(std::mem::take(map));
    let result = super::edit::write_at_path(&mut root, keys, value);
    let Value::Map(walked) = root else {
        unreachable!("the root stays a map")
    };
    *map = walked;
    result
}

/// The mapping at `keys` in `map`, when every segment resolves to one.
fn map_at<'a>(map: &'a Map, keys: &[&str]) -> Option<&'a Map> {
    let mut current = map;
    for key in keys {
        match current.get(*key) {
            Some(Value::Map(next)) => current = next,
            _ => return None,
        }
    }
    Some(current)
}

/// Remove `keys` from `map`; `false` when the path was already absent.
/// The same root adapter shape as [`set_in_tree`].
fn remove_from_tree(map: &mut Map, keys: &[&str]) -> bool {
    let mut root = Value::Map(std::mem::take(map));
    let removed = super::edit::unset_at_path(&mut root, keys);
    let Value::Map(walked) = root else {
        unreachable!("the root stays a map")
    };
    *map = walked;
    removed
}

/// Parse the file under edit into its value tree. Empty and comments-only
/// sources are the empty map (same rule as [`YamlAdapter::parse`]).
fn parse_edit_source(source: &str) -> Result<Map, FormatError> {
    match YamlAdapter.parse(source)?.value {
        Value::Map(map) => Ok(map),
        other => Err(FormatError::Edit {
            format: "yaml",
            message: format!(
                "cannot edit a document whose root is a {}, not a map",
                other.type_str()
            ),
        }),
    }
}

/// Build the yamlpath route for `keys` (owning its components, so patch
/// lists never borrow from intermediate key buffers).
fn route_for(keys: &[&str]) -> yamlpath::Route<'static> {
    yamlpath::Route::from(
        keys.iter()
            .map(|k| yamlpath::Component::Key(std::borrow::Cow::Owned((*k).to_string())))
            .collect::<Vec<_>>(),
    )
}

/// Emit the patch sequence that creates `keys` (relative to the existing
/// mapping at `prefix`) with `value` at the leaf.
///
/// `yamlpatch`'s `Add` renders a mapping nested inside a mapping with
/// broken indentation, so map values are decomposed into one `Add` per
/// level — each level lands as an (empty, then extended) mapping, which
/// the patcher handles correctly in both block and flow styles.
fn push_create_patches(
    patches: &mut Vec<yamlpatch::Patch<'static>>,
    prefix: &[&str],
    key: &str,
    value: &Value,
) {
    match value {
        Value::Map(map) if !map.is_empty() => {
            patches.push(yamlpatch::Patch {
                route: route_for(prefix),
                operation: yamlpatch::Op::Add {
                    key: key.to_string(),
                    value: yaml_serde::Value::Mapping(yaml_serde::Mapping::new()),
                },
            });
            let child_prefix: Vec<&str> = prefix.iter().copied().chain([key]).collect();
            for (k, v) in map {
                // Keys borrow from `value`, which outlives the patch list.
                push_create_patches(patches, &child_prefix, k, v);
            }
        }
        _ => patches.push(yamlpatch::Patch {
            route: route_for(prefix),
            operation: yamlpatch::Op::Add {
                key: key.to_string(),
                value: value_to_patch(value),
            },
        }),
    }
}

/// Apply `patches` to `source` and verify the result reparses to exactly
/// `expected`. A patch-level failure is a typed edit error; a verification
/// mismatch is the typed refusal — the shape is one the patch stack cannot
/// rewrite honestly (ADR-0002's known-refusals row).
fn apply_and_verify(
    source: &str,
    patches: &[yamlpatch::Patch<'_>],
    expected: &Map,
    display_path: &str,
    operation: Operation,
) -> Result<String, FormatError> {
    let doc = yamlpath::Document::new(source.to_string()).map_err(|e| FormatError::Parse {
        format: "yaml",
        message: e.to_string(),
        span: None,
    })?;
    let patched = yamlpatch::apply_yaml_patches(&doc, patches).map_err(|e| FormatError::Edit {
        format: "yaml",
        message: format!("cannot edit '{display_path}': {e}"),
    })?;
    let result = patched.source().to_string();
    let reparsed = parse_edit_source(&result)?;
    if trees_equal(&Value::Map(reparsed), &Value::Map(expected.clone())) {
        Ok(result)
    } else {
        Err(UnsupportedByFormat {
            format: "yaml",
            operation,
        }
        .into())
    }
}

/// Set `value` at `keys` in `source`, span-preserving. Replaces an existing
/// scalar in place; container replacement re-emits the key (remove + add);
/// missing paths are created level by level. Every outcome is verified —
/// see [`apply_and_verify`].
fn set_in_source(
    source: &str,
    keys: &[&str],
    value: &Value,
    operation: Operation,
) -> Result<String, FormatError> {
    let original = parse_edit_source(source)?;
    let mut expected = original.clone();
    set_in_tree(&mut expected, keys, as_parsed(value))?;
    let display_path = keys.join(".");

    // A blank or comments-only file (the template-seeded create-file
    // case): insert the serialized subtree into the document body. This
    // branch is syntactic, not semantic — a source that merely PARSES to
    // an empty map, like `{}`, still has a document root, and appending
    // after it would produce a second one; those sources ride the patch
    // path below instead. Content must land INSIDE the document: after
    // any `---` start marker but before a `...` end marker — text after
    // `...` sits outside the document and fails to parse. The insert
    // carries the same promise as every patched edit: the result must
    // reparse to exactly the intended tree, or the edit refuses typed.
    if is_blank_or_comments(source) {
        let mut fresh = Map::new();
        set_in_tree(&mut fresh, keys, value.clone())?;
        let mut rendered = YamlAdapter.serialize(&Value::Map(fresh))?;
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        let insert_at = end_marker_offset(source).unwrap_or(source.len());
        let mut out = String::with_capacity(source.len() + rendered.len() + 1);
        out.push_str(&source[..insert_at]);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&rendered);
        out.push_str(&source[insert_at..]);
        let reparsed = parse_edit_source(&out)?;
        if trees_equal(&Value::Map(reparsed), &Value::Map(expected)) {
            return Ok(out);
        }
        return Err(UnsupportedByFormat {
            format: "yaml",
            operation,
        }
        .into());
    }

    // Walk the existing tree to find how much of the path is already
    // there; the first missing key starts the create chain.
    let mut existing = 0usize;
    let mut cursor: &Map = &original;
    for key in keys {
        match cursor.get(*key) {
            Some(Value::Map(next)) if existing + 1 < keys.len() => {
                cursor = next;
                existing += 1;
            }
            Some(_) => {
                existing += 1;
                break;
            }
            None => break,
        }
    }

    let mut patches = Vec::new();
    if existing == keys.len() {
        // The full path exists: scalars replace in place; containers
        // cannot ride `Op::Replace` (the patcher rejects them), so the
        // key is re-emitted — removed and added back with the new value.
        match value {
            Value::Array(_) | Value::Map(_) => {
                let (leaf, parents) = keys.split_last().expect("checked non-empty");
                patches.push(yamlpatch::Patch {
                    route: route_for(keys),
                    operation: yamlpatch::Op::Remove,
                });
                push_create_patches(&mut patches, parents, leaf, value);
            }
            _ => patches.push(yamlpatch::Patch {
                route: route_for(keys),
                operation: yamlpatch::Op::Replace(value_to_patch(value)),
            }),
        }
    } else {
        // Path partially exists. The prefix ends on a map — a scalar in
        // the way already errored as a path conflict in set_in_tree above
        // — and the remainder is created one level at a time.
        let prefix = &keys[..existing];
        let remainder = &keys[existing..];
        let mut nested = as_parsed(value);
        for key in remainder[1..].iter().rev() {
            let mut level = Map::new();
            level.insert(key.to_string(), nested);
            nested = Value::Map(level);
        }
        push_create_patches(&mut patches, prefix, remainder[0], &nested);
    }

    apply_and_verify(source, &patches, &expected, &display_path, operation)
}

/// Remove `keys` from `source`. A missing path is a no-op — the source is
/// returned unchanged, mirroring the TOML adapter. Removing the sole child
/// of a nested mapping rewrites the emptied parent to an explicit `{}` —
/// a bare `parent:` left behind would reparse as null, the value this
/// adapter rejects — matching the TOML adapter, whose emptied tables keep
/// their header.
fn unset_in_source(source: &str, keys: &[&str]) -> Result<String, FormatError> {
    let original = parse_edit_source(source)?;
    let mut expected = original.clone();
    if !remove_from_tree(&mut expected, keys) {
        return Ok(source.to_string());
    }
    let (_, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    // Removing one leaf can empty only its immediate parent. An emptied
    // non-root parent becomes an explicit `{}` via `Op::Replace` — the
    // one container value the patcher replaces correctly (non-empty
    // containers are re-emitted by the set path instead). An emptied
    // ROOT needs nothing: the leaf's removal leaves a blank document,
    // which already reparses to the empty map.
    let emptied_parent =
        !parents.is_empty() && map_at(&expected, parents).is_some_and(|m| m.is_empty());
    let patches = if emptied_parent {
        [yamlpatch::Patch {
            route: route_for(parents),
            operation: yamlpatch::Op::Replace(yaml_serde::Value::Mapping(
                yaml_serde::Mapping::new(),
            )),
        }]
    } else {
        [yamlpatch::Patch {
            route: route_for(keys),
            operation: yamlpatch::Op::Remove,
        }]
    };
    apply_and_verify(
        source,
        &patches,
        &expected,
        &keys.join("."),
        Operation::EditUnset,
    )
}

// --- template emission (native YAML comments) -----------------------------

/// `true` when the schema subtree emits at least one uncommented line — a
/// leaf with a default. A mapping key whose lines are all commented must
/// itself be commented, or the generated document would carry a null.
fn has_active_content(schema: &Schema) -> bool {
    schema.fields.iter().any(|nf| match &nf.field {
        Field::Leaf(leaf) => leaf.default.is_some(),
        Field::Nested(child) => has_active_content(child),
        // Array-of and map-of sections render as fully commented examples.
        Field::ArrayOf(_) | Field::MapOf(_) => false,
    })
}

/// The YAML template renderer: `key: value` lines scoped by indentation.
/// The context is the indentation depth (0 at the root). Every
/// schema-derived mapping key renders through [`inline_scalar`], so field
/// names the parser would misread bare (`#token`, `a: b`, `*alias`) land
/// quoted and the generated document stays parseable.
struct YamlTemplate;

impl TemplateRenderer for YamlTemplate {
    type Ctx = usize;
    type Out = String;

    // The same layout as the TOML template, so cross-format templates read
    // alike. (YAML's indentation scoping does not require it.)
    const LEAVES_FIRST: bool = true;

    fn leaf(
        &mut self,
        out: &mut String,
        depth: &usize,
        name: &str,
        leaf: &Leaf,
    ) -> Result<(), FormatError> {
        let indent = "  ".repeat(*depth);
        for line in leaf_annotations(leaf, "YAML", &mut |v| Ok(format_inline_yaml(v)))? {
            push_comment_line(out, &indent, &line);
        }
        match &leaf.default {
            Some(value) => {
                let _ = writeln!(
                    out,
                    "{indent}{}: {}",
                    inline_scalar(name),
                    format_inline_yaml(value)
                );
            }
            None => {
                let hint = placeholder(&leaf.ty, "''", "1970-01-01T00:00:00Z");
                let _ = writeln!(out, "{indent}#{}: {hint}", inline_scalar(name));
            }
        }
        out.push('\n');
        Ok(())
    }

    fn nested(
        &mut self,
        out: &mut String,
        depth: &usize,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        let indent = "  ".repeat(*depth);
        for line in &child.doc {
            push_comment_line(out, &indent, line);
        }
        if has_active_content(child) {
            let _ = writeln!(out, "{indent}{}:", inline_scalar(name));
            walk_level(self, child, &(depth + 1), out)
        } else {
            // All-commented section: comment the key too, or the
            // generated document would parse it as null.
            let mut buf = String::new();
            let _ = writeln!(buf, "{indent}{}:", inline_scalar(name));
            walk_level(self, child, &(depth + 1), &mut buf)?;
            push_commented_block(out, &buf);
            Ok(())
        }
    }

    fn array_of(
        &mut self,
        out: &mut String,
        depth: &usize,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        let indent = "  ".repeat(*depth);
        for line in &child.doc {
            push_comment_line(out, &indent, line);
        }
        let mut buf = String::new();
        let _ = writeln!(buf, "{indent}{}:", inline_scalar(name));
        let mut item = String::new();
        walk_level(self, child, &(depth + 2), &mut item)?;
        buf.push_str(&with_sequence_dash(&item, depth + 1));
        push_commented_block(out, &buf);
        Ok(())
    }

    fn map_of(
        &mut self,
        out: &mut String,
        depth: &usize,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        let indent = "  ".repeat(*depth);
        for line in &child.doc {
            push_comment_line(out, &indent, line);
        }
        let mut buf = String::new();
        let _ = writeln!(buf, "{indent}{}:", inline_scalar(name));
        let _ = writeln!(buf, "{indent}  <key>:");
        walk_level(self, child, &(depth + 2), &mut buf)?;
        push_commented_block(out, &buf);
        Ok(())
    }
}

/// Turn a rendered mapping block into a sequence item: the first content
/// line's indentation gains the `- ` marker at `dash_depth`.
fn with_sequence_dash(block: &str, dash_depth: usize) -> String {
    let dash_col = dash_depth * 2;
    let mut out = String::new();
    let mut dashed = false;
    for line in block.lines() {
        if !dashed && !line.is_empty() && !line.trim_start().starts_with('#') {
            let (head, tail) = line.split_at(dash_col + 2);
            debug_assert!(head.trim().is_empty(), "content lines start indented");
            out.push_str(&" ".repeat(dash_col));
            out.push_str("- ");
            out.push_str(tail);
            dashed = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Format an owned [`Value`] as it would appear inline in a YAML file:
/// flow-style containers, scalars in the same spelling `serde_norway`
/// would emit (so quoting rules match the parser exactly).
fn format_inline_yaml(value: &Value) -> String {
    match value {
        Value::String(s) => inline_scalar(s),
        Value::Integer(i) => i.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Float(_) | Value::Datetime(_) => inline_norway(&value_to_norway(value)),
        Value::Array(items) => {
            let listed = items
                .iter()
                .map(format_inline_yaml)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{listed}]")
        }
        Value::Map(map) => {
            let listed = map
                .iter()
                .map(|(k, v)| format!("{}: {}", inline_scalar(k), format_inline_yaml(v)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{listed}}}")
        }
    }
}

/// One string scalar in `serde_norway`'s own spelling (quoted only when
/// the parser needs it). Strings with control characters fall back to a
/// JSON string literal — valid YAML escaping, since YAML 1.2's
/// double-quoted style is a JSON superset — where the library would emit
/// a block scalar, unusable inline.
fn inline_scalar(s: &str) -> String {
    if s.chars().any(|c| c.is_control()) {
        return json_escaped(s);
    }
    inline_norway(&serde_norway::Value::String(s.to_string()))
}

/// `s` as a one-line JSON string literal. Rust's `{:?}` escaping is NOT
/// valid YAML (`\u{1}` vs JSON/YAML's `\u0001`), so this is the inline
/// spelling for strings whose control characters need escaping. All
/// control characters are in the Basic Multilingual Plane, so the
/// four-digit `\uXXXX` form always suffices.
fn json_escaped(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::super::SetTarget;
    use super::*;

    // --- display spelling ------------------------------------------------

    #[test]
    fn display_entry_yaml_quotes_non_plain_keys() {
        // The ordinary dotted path stays a bare plain scalar…
        assert_eq!(
            YamlAdapter.display_entry("server.host", "localhost"),
            "server.host: localhost"
        );
        // …and a key YAML cannot carry as a plain scalar comes out in a
        // one-line quoted spelling: the rendered key half must
        // round-trip back to the original name, and the assignment
        // stays on a single line (serde_norway would emit a block
        // scalar for a newline, which would swallow the `: value`).
        for key in [
            "a: b", "a # b", "'a'", "\"a\"", "- a", "a\nb", "a\rb", "a\tb", "a\u{1}b",
        ] {
            let rendered = YamlAdapter.display_entry(key, "1");
            assert!(
                !rendered.contains('\n') && !rendered.contains('\r'),
                "display must stay one line, got {rendered:?}"
            );
            let encoded = rendered
                .strip_suffix(": 1")
                .unwrap_or_else(|| panic!("no value half in {rendered:?}"));
            let round_tripped: String =
                serde_norway::from_str(encoded).expect("encoded key must parse as one scalar");
            assert_eq!(round_tripped, key, "key {key:?} rendered as {rendered:?}");
        }
    }

    fn parse_map(text: &str) -> Map {
        match YamlAdapter.parse(text).expect("fixture must parse").value {
            Value::Map(map) => map,
            other => panic!("expected map root, got {other:?}"),
        }
    }

    fn parse_err(text: &str) -> String {
        match YamlAdapter.parse(text).expect_err("fixture must fail") {
            FormatError::Parse {
                format, message, ..
            } => {
                assert_eq!(format, "yaml");
                message
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    // --- baseline mapping rows (ADR-0002) ---

    #[test]
    fn parse_scalars_and_containers() {
        let map = parse_map("s: x\ni: 3\nf: 1.5\nb: true\nt:\n  n: 1\n  arr: [1, 2]\n");
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
    fn strict_scalars_no_norway_problem() {
        // Only true/false spellings are booleans (ADR-0003's source-verified
        // stance): the YAML 1.1 bool spellings stay strings.
        let map = parse_map("country: no\nagree: yes\npower: on\nlight: off\nreal: true\n");
        assert_eq!(map["country"], Value::String("no".into()));
        assert_eq!(map["agree"], Value::String("yes".into()));
        assert_eq!(map["power"], Value::String("on".into()));
        assert_eq!(map["light"], Value::String("off".into()));
        assert_eq!(map["real"], Value::Boolean(true));
    }

    #[test]
    fn aliases_resolve_invisibly() {
        let map = parse_map("base: &shared 42\ncopy: *shared\n");
        assert_eq!(map["base"], Value::Integer(42));
        assert_eq!(map["copy"], Value::Integer(42));
    }

    #[test]
    fn custom_tags_are_typed_errors_naming_the_key() {
        let message = parse_err("section:\n  key: !custom foo\n");
        assert!(message.contains("tag"), "message: {message}");
        assert!(message.contains("section.key"), "message: {message}");
    }

    #[test]
    fn merge_keys_are_typed_errors() {
        let message = parse_err("base: &b\n  x: 1\nchild:\n  <<: *b\n  y: 2\n");
        assert!(message.contains("merge key"), "message: {message}");
        assert!(message.contains("child"), "message: {message}");
    }

    #[test]
    fn null_is_a_typed_error_advising_absence() {
        for source in ["empty: null\n", "empty: ~\n", "section:\n  empty:\n"] {
            let message = parse_err(source);
            assert!(message.contains("null"), "message: {message}");
            assert!(
                message.contains("absence expresses unset"),
                "message: {message}"
            );
            assert!(message.contains("empty"), "message: {message}");
        }
    }

    #[test]
    fn empty_and_comments_only_documents_are_the_empty_map() {
        // Absence, not null: a blank or fully commented file is an empty
        // config — the case every all-commented generated template hits.
        assert_eq!(YamlAdapter.parse("").unwrap().value, Value::Map(Map::new()));
        assert_eq!(
            YamlAdapter.parse("# just\n\n# comments\n").unwrap().value,
            Value::Map(Map::new())
        );
    }

    #[test]
    fn bare_document_markers_are_the_empty_map() {
        // `---` alone is a null document and `...` alone a parse error in
        // serde_norway; both are "no content" here — absence, not null.
        for source in ["---\n", "...\n", "---\n# note\n", "# note\n---\n"] {
            assert_eq!(
                YamlAdapter.parse(source).unwrap().value,
                Value::Map(Map::new()),
                "source: {source:?}"
            );
        }
        // A marker followed by real content is NOT blank: it parses (or
        // errors) through serde_norway as usual.
        let map = parse_map("---\nfoo: 1\n");
        assert_eq!(map["foo"], Value::Integer(1));
    }

    #[test]
    fn non_string_keys_are_typed_errors() {
        let message = parse_err("section:\n  1: x\n");
        assert!(message.contains("non-string"), "message: {message}");
        assert!(message.contains("section"), "message: {message}");
    }

    #[test]
    fn integers_outside_i64_are_typed_errors() {
        let message = parse_err("big: 9223372036854775808\n");
        assert!(message.contains("out of range"), "message: {message}");
        assert!(message.contains("big"), "message: {message}");
        // i64::MAX itself is fine.
        let map = parse_map("max: 9223372036854775807\n");
        assert_eq!(map["max"], Value::Integer(i64::MAX));
    }

    #[test]
    fn integers_beyond_u64_or_below_i64_min_name_the_key() {
        // These are rejected inside serde_norway, before this adapter's
        // own range check ever sees them — but its error still carries
        // the full dotted path to the offending key, plus a span.
        let message = parse_err("small: -9223372036854775809\n");
        assert!(message.contains("small"), "message: {message}");
        let message = parse_err("big: 18446744073709551616\n");
        assert!(message.contains("big"), "message: {message}");
        let message = parse_err("section:\n  big: 18446744073709551616\n");
        assert!(message.contains("section.big"), "message: {message}");
    }

    #[test]
    fn non_finite_floats_are_accepted() {
        let map = parse_map("a: .inf\nb: -.inf\nc: .nan\n");
        assert_eq!(map["a"], Value::Float(f64::INFINITY));
        assert_eq!(map["b"], Value::Float(f64::NEG_INFINITY));
        assert!(map["c"].as_float().unwrap().is_nan());
    }

    #[test]
    fn datetimes_arrive_as_strings_for_schema_driven_coercion() {
        // The adapter never guesses datetimes; the four TOML lexical forms
        // stay strings here and the schema pass coerces them (ADR-0001).
        let map = parse_map(
            "offset: 1979-05-27T07:32:00Z\nlocal: 1979-05-27T07:32:00\ndate: 1979-05-27\ntime: 07:32:00\n",
        );
        for key in ["offset", "local", "date", "time"] {
            assert!(
                matches!(map[key], Value::String(_)),
                "{key} should parse as a string, got {:?}",
                map[key]
            );
        }
    }

    #[test]
    fn parse_error_carries_message_and_span() {
        let err = YamlAdapter.parse("a: [1, 2\n").unwrap_err();
        match err {
            FormatError::Parse {
                format,
                message,
                span,
            } => {
                assert_eq!(format, "yaml");
                assert!(!message.is_empty());
                assert!(span.is_some(), "syntax errors report a location");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_spans_stay_on_char_boundaries() {
        // The reported span must be sliceable: start and end land on char
        // boundaries even when the error location neighbours multibyte
        // characters.
        for source in [
            "é: \"open\n",
            "aé: [1,\n",
            "ключ: [1,\n",
            "a: 1\nя: @x\n",
            "k:\n\tя: 1\n",
        ] {
            let err = YamlAdapter.parse(source).expect_err("fixture must fail");
            let FormatError::Parse { span, .. } = err else {
                panic!("expected Parse for {source:?}");
            };
            let Some(span) = span else { continue };
            assert!(span.end <= source.len(), "span past EOF for {source:?}");
            assert!(
                source.is_char_boundary(span.start) && source.is_char_boundary(span.end),
                "span {span:?} splits a character in {source:?}"
            );
        }
    }

    // --- span index (ADR-0005 / ADR-0006 / ADR-0008) ---

    fn snippet(source: &str, span: Span) -> &str {
        &source[span.start..span.end]
    }

    fn assert_covers_tree(value: &Value, spans: &BTreeMap<ConfigPath, SpanEntry>) {
        fn walk(value: &Value, path: ConfigPath, spans: &BTreeMap<ConfigPath, SpanEntry>) {
            if !path.segments().is_empty() {
                assert!(spans.contains_key(&path), "span index missing path {path}");
            }
            match value {
                Value::Map(map) => {
                    for (key, child) in map {
                        walk(child, path.clone().key(key), spans);
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(child, path.clone().index(i), spans);
                    }
                }
                _ => {}
            }
        }
        walk(value, ConfigPath::new(), spans);
    }

    #[test]
    fn empty_document_span_index_is_empty() {
        // An empty map has no child paths; the root is not an index entry.
        assert!(YamlAdapter.parse("").unwrap().spans.is_empty());
        assert!(
            YamlAdapter
                .parse("# just\n\n# comments\n")
                .unwrap()
                .spans
                .is_empty()
        );
        assert!(YamlAdapter.parse("{}\n").unwrap().spans.is_empty());
    }

    #[test]
    fn span_index_covers_nested_maps() {
        let source = "outer:\n  inner:\n    extra: 1\n    keep: 2\n";
        let parsed = YamlAdapter.parse(source).unwrap();
        assert_covers_tree(&parsed.value, &parsed.spans);

        let extra = parsed
            .spans
            .get(&ConfigPath::new().key("outer").key("inner").key("extra"))
            .expect("nested map key must be indexed");
        assert_eq!(
            snippet(source, extra.key.expect("map keys have a key span")),
            "extra"
        );
        assert_eq!(snippet(source, extra.value), "1");

        let outer = parsed
            .spans
            .get(&ConfigPath::new().key("outer"))
            .expect("parent map must be indexed");
        assert_eq!(
            snippet(source, outer.key.expect("map keys have a key span")),
            "outer"
        );
        assert!(
            snippet(source, outer.value).contains("inner:"),
            "parent value span covers the nested mapping, got {:?}",
            snippet(source, outer.value)
        );
    }

    #[test]
    fn span_index_covers_unknown_key_in_inline_table() {
        // Demoable: a YAML unknown key in an inline table has a correct
        // key span. WS02 owns wiring this into unknown-key errors.
        let source = "server: { host: localhost, extra: 1 }\n";
        let parsed = YamlAdapter.parse(source).unwrap();
        assert_covers_tree(&parsed.value, &parsed.spans);

        let extra = parsed
            .spans
            .get(&ConfigPath::new().key("server").key("extra"))
            .expect("inline-table key must be indexed");
        assert_eq!(
            snippet(source, extra.key.expect("map keys have a key span")),
            "extra"
        );
        assert_eq!(snippet(source, extra.value), "1");

        let host = parsed
            .spans
            .get(&ConfigPath::new().key("server").key("host"))
            .expect("inline-table sibling must be indexed");
        assert_eq!(
            snippet(source, host.key.expect("map keys have a key span")),
            "host"
        );
        assert_eq!(snippet(source, host.value), "localhost");
    }

    #[test]
    fn span_index_covers_arrays_and_array_of_maps() {
        let source = "\
items:
  - name: a
  - name: b
arr: [1, 2]
flow: [{ host: a }, { host: b, extra: 1 }]
";
        let parsed = YamlAdapter.parse(source).unwrap();
        assert_covers_tree(&parsed.value, &parsed.spans);

        let first = parsed
            .spans
            .get(&ConfigPath::new().key("items").index(0))
            .expect("array element must be indexed");
        assert!(first.key.is_none(), "array elements have no key token");
        assert!(
            snippet(source, first.value).contains("name: a"),
            "block sequence item value, got {:?}",
            snippet(source, first.value)
        );

        let name = parsed
            .spans
            .get(&ConfigPath::new().key("items").index(0).key("name"))
            .expect("array-of-maps key must be indexed");
        assert_eq!(
            snippet(source, name.key.expect("map keys have a key span")),
            "name"
        );
        assert_eq!(snippet(source, name.value), "a");

        let arr0 = parsed
            .spans
            .get(&ConfigPath::new().key("arr").index(0))
            .expect("flow sequence item must be indexed");
        assert!(arr0.key.is_none());
        assert_eq!(snippet(source, arr0.value), "1");

        let extra = parsed
            .spans
            .get(&ConfigPath::new().key("flow").index(1).key("extra"))
            .expect("flow array-of-maps key must be indexed");
        assert_eq!(
            snippet(source, extra.key.expect("map keys have a key span")),
            "extra"
        );
        assert_eq!(snippet(source, extra.value), "1");
    }

    #[test]
    fn alias_expanded_paths_caret_the_alias_token() {
        // ADR-0008: a path that exists in source keeps yamlpath's ranges;
        // a path that exists in Value only because an alias expanded
        // carets the `*name` token, not the anchor's nested keys.
        let source = "\
defaults: &defaults
  host: localhost
  port: 5432
db: *defaults
";
        let parsed = YamlAdapter.parse(source).unwrap();
        assert_covers_tree(&parsed.value, &parsed.spans);

        let written = parsed
            .spans
            .get(&ConfigPath::new().key("defaults").key("host"))
            .expect("anchor-side path exists in source");
        assert_eq!(
            snippet(source, written.key.expect("map keys have a key span")),
            "host"
        );
        assert_eq!(snippet(source, written.value), "localhost");

        let db = parsed
            .spans
            .get(&ConfigPath::new().key("db"))
            .expect("alias assignment exists in source");
        assert_eq!(
            snippet(source, db.key.expect("map keys have a key span")),
            "db"
        );
        assert_eq!(snippet(source, db.value), "*defaults");

        for key in ["host", "port"] {
            let entry = parsed
                .spans
                .get(&ConfigPath::new().key("db").key(key))
                .unwrap_or_else(|| panic!("expanded path db.{key} must be indexed"));
            assert_eq!(
                snippet(
                    source,
                    entry.key.expect("expanded path uses the alias as key")
                ),
                "*defaults",
                "db.{key} key span"
            );
            assert_eq!(
                snippet(source, entry.value),
                "*defaults",
                "db.{key} value span"
            );
        }
    }

    #[test]
    fn alias_array_item_and_its_expanded_children_caret_the_token() {
        let source = "\
tmpl: &t
  host: x
servers:
  - *t
";
        let parsed = YamlAdapter.parse(source).unwrap();
        assert_covers_tree(&parsed.value, &parsed.spans);

        let item = parsed
            .spans
            .get(&ConfigPath::new().key("servers").index(0))
            .expect("alias sequence item must be indexed");
        assert!(
            item.key.is_none(),
            "written array elements have no key token"
        );
        assert_eq!(snippet(source, item.value), "*t");

        let host = parsed
            .spans
            .get(&ConfigPath::new().key("servers").index(0).key("host"))
            .expect("expanded child of an alias item must be indexed");
        assert_eq!(
            snippet(
                source,
                host.key.expect("expanded path uses the alias as key")
            ),
            "*t"
        );
        assert_eq!(snippet(source, host.value), "*t");
    }

    // --- serialization ---

    #[test]
    fn serialize_round_trips_parse() {
        let source = "b: true\ni: 3\ns: x\nt:\n  n: 1\n";
        let value = YamlAdapter.parse(source).unwrap().value;
        let text = YamlAdapter.serialize(&value).unwrap();
        let reparsed = YamlAdapter.parse(&text).unwrap().value;
        assert_eq!(value, reparsed);
    }

    #[test]
    fn serialize_quotes_strings_the_parser_would_mistype() {
        let mut map = Map::new();
        map.insert("port_str".into(), Value::String("8080".into()));
        map.insert("country".into(), Value::String("no".into()));
        let text = YamlAdapter.serialize(&Value::Map(map.clone())).unwrap();
        assert_eq!(YamlAdapter.parse(&text).unwrap().value, Value::Map(map));
    }

    #[test]
    fn serialize_handles_non_finite_floats() {
        let mut map = Map::new();
        map.insert("inf".into(), Value::Float(f64::INFINITY));
        map.insert("ninf".into(), Value::Float(f64::NEG_INFINITY));
        let text = YamlAdapter.serialize(&Value::Map(map)).unwrap();
        assert!(text.contains(".inf"), "text: {text}");
        let reparsed = parse_map(&text);
        assert_eq!(reparsed["inf"], Value::Float(f64::INFINITY));
        assert_eq!(reparsed["ninf"], Value::Float(f64::NEG_INFINITY));
    }

    #[test]
    fn serialize_datetime_as_lexical_string() {
        let mut map = Map::new();
        map.insert(
            "when".into(),
            Value::Datetime("1979-05-27T07:32:00Z".parse().unwrap()),
        );
        let text = YamlAdapter.serialize(&Value::Map(map)).unwrap();
        assert_eq!(text, "when: 1979-05-27T07:32:00Z\n");
    }

    #[test]
    fn serialize_hand_constructed_invalid_datetime_never_panics() {
        // `Datetime`'s component fields are public (`toml_datetime`'s
        // types); a hand-assembled non-grammatical value serializes as
        // its `Display` string — garbage in, garbage out, never a panic.
        use crate::value::{Date, Datetime};
        let mut map = Map::new();
        map.insert(
            "when".into(),
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
        let text = YamlAdapter.serialize(&Value::Map(map)).unwrap();
        assert_eq!(text, "when: 1979-13-01\n");
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
            "when".into(),
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
        let text = YamlAdapter.serialize(&Value::Map(map)).unwrap();
        assert!(text.contains("1979-05-27T07:32:00-546:08"), "{text}");
    }

    // --- template ---

    #[test]
    fn template_matches_golden() {
        // Mirrors the TOML adapter's golden schema, so the two templates
        // can be compared side by side.
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
        let golden = r#"# Demo runtime schema

# App host
host: localhost

# Port number
port: 8080

# Log verbosity
# Allowed: debug | info
level: info

# Required name.
# Required.
#name: ''

# Any value.
# Accepts: any YAML value
# Required.
#rule: ''

# Database settings
db:
  #url: ''

  pool_size: 5

"#;
        assert_eq!(YamlAdapter.template(&schema).unwrap(), golden);
    }

    #[test]
    fn template_round_trips_through_parse() {
        // A generated template must be a valid config document: defaults
        // present, commented placeholders absent — never a null.
        let schema = crate::fixtures::test::test_schema();
        let text = YamlAdapter.template(&schema).unwrap();
        let map = parse_map(&text);
        assert_eq!(map["host"], Value::String("localhost".into()));
        assert_eq!(map["port"], Value::Integer(8080));
        assert_eq!(map["debug"], Value::Boolean(false));
        let db = map["database"].as_map().unwrap();
        assert_eq!(db["pool_size"], Value::Integer(5));
        assert!(
            !db.contains_key("url"),
            "commented placeholder must not parse"
        );
    }

    #[test]
    fn template_comments_out_sections_with_no_defaults() {
        // A section whose every line is commented must have its key
        // commented too — otherwise the template would parse it as null.
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field("port", Field::integer().default(1i64))
            .nested(
                "auth",
                RtSchema::object("Auth")
                    .doc("Optional auth settings")
                    .field("token", Field::string().optional()),
            )
            .build();
        let text = YamlAdapter.template(&schema).unwrap();
        assert!(text.contains("#auth:"), "text: {text}");
        assert!(text.contains("#  #token: ''"), "text: {text}");
        let map = parse_map(&text);
        assert!(!map.contains_key("auth"));
    }

    #[test]
    fn template_renders_array_of_and_map_of_examples_commented() {
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field("port", Field::integer().default(1i64))
            .array_of(
                "plugins",
                RtSchema::object("Plugin")
                    .doc("One plugin per entry.")
                    .field("name", Field::string())
                    .field("path", Field::string().default("/usr/lib")),
            )
            .map_of(
                "limits",
                RtSchema::object("Limit").field("burst", Field::integer().default(2i64)),
            )
            .build();
        let text = YamlAdapter.template(&schema).unwrap();
        assert!(text.contains("#plugins:"), "text: {text}");
        assert!(
            text.contains("#  - path: /usr/lib"),
            "sequence example carries a dash on the first content line: {text}"
        );
        assert!(
            text.contains("#    #name: ''"),
            "defaultless item leaves stay placeholder comments: {text}"
        );
        assert!(text.contains("#limits:"), "text: {text}");
        assert!(text.contains("#  <key>:"), "text: {text}");
        // The commented examples must not leak into the parsed document.
        let map = parse_map(&text);
        assert_eq!(map.keys().collect::<Vec<_>>(), ["port"]);
    }

    #[test]
    fn template_escapes_keys_the_parser_would_misread() {
        // Schema names only forbid empty, `.`, `[`, `]` — a template must
        // quote names the parser would otherwise read as comments,
        // aliases, or nested mappings.
        use crate::runtime::{Field, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .field("#token", Field::string().default("x"))
            .field("a: b", Field::integer().default(1i64))
            .field("*alias", Field::string())
            .nested(
                "d: b",
                RtSchema::object("Db").field("pool", Field::integer().default(5i64)),
            )
            .build();
        let text = YamlAdapter.template(&schema).unwrap();
        let map = parse_map(&text);
        assert_eq!(map["#token"], Value::String("x".into()));
        assert_eq!(map["a: b"], Value::Integer(1));
        assert!(
            !map.contains_key("*alias"),
            "defaultless leaf stays a commented placeholder: {text}"
        );
        assert_eq!(map["d: b"].as_map().unwrap()["pool"], Value::Integer(5));
    }

    #[test]
    fn inline_scalar_control_characters_are_valid_yaml() {
        for s in ["a\u{1}b", "tab\there", "line\nbreak", "del\u{7f}"] {
            let doc = format!("k: {}\n", inline_scalar(s));
            let map = parse_map(&doc);
            assert_eq!(map["k"], Value::String(s.to_string()), "doc: {doc:?}");
        }
    }

    // --- edits ---

    fn set_edit<'a>(path: &'a ConfigPath, value: &'a Value, target: SetTarget) -> FileEdit<'a> {
        FileEdit::Set {
            path,
            value,
            target,
        }
    }

    #[test]
    fn edit_set_replaces_scalar_preserving_bytes_outside_the_span() {
        let source = "# top comment\nhost: example.com  # inline note\nport: 8080\n\n# db section\ndatabase:\n  url: pg://x\n  pool_size: 5\n";
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(3000);
        let out = YamlAdapter
            .edit(source, set_edit(&path, &value, SetTarget::ExistingValue))
            .unwrap();
        // Only the port line changed; every other byte survives.
        assert_eq!(
            out,
            "# top comment\nhost: example.com  # inline note\nport: 3000\n\n# db section\ndatabase:\n  url: pg://x\n  pool_size: 5\n"
        );
    }

    #[test]
    fn edit_set_replaces_nested_scalar_keeping_sibling_comments() {
        let source = "server:\n  # inner comment\n  port: 8080  # trailing\n  host: x\n";
        let path = ConfigPath::new().key("server").key("port");
        let value = Value::Integer(9090);
        let out = YamlAdapter
            .edit(source, set_edit(&path, &value, SetTarget::ExistingValue))
            .unwrap();
        assert_eq!(
            out,
            "server:\n  # inner comment\n  port: 9090  # trailing\n  host: x\n"
        );
    }

    #[test]
    fn edit_set_creates_missing_path() {
        let source = "port: 8080\n";
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let out = YamlAdapter
            .edit(source, set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert!(out.starts_with("port: 8080\n"), "out: {out}");
        let map = parse_map(&out);
        assert_eq!(
            map["database"].as_map().unwrap()["url"],
            Value::String("pg://x".into())
        );
    }

    #[test]
    fn edit_set_creates_missing_key_under_existing_section() {
        let source = "# note\ndatabase:\n  url: pg://x\n";
        let path = ConfigPath::new().key("database").key("pool_size");
        let value = Value::Integer(10);
        let out = YamlAdapter
            .edit(source, set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert!(out.contains("# note"), "out: {out}");
        assert!(out.contains("url: pg://x"), "out: {out}");
        let map = parse_map(&out);
        assert_eq!(
            map["database"].as_map().unwrap()["pool_size"],
            Value::Integer(10)
        );
    }

    #[test]
    fn edit_set_replaces_container_value() {
        let source = "items: [1, 2]\nport: 1\n";
        let path = ConfigPath::new().key("items");
        let value = Value::Array(vec![Value::Integer(3), Value::Integer(4)]);
        let out = YamlAdapter
            .edit(source, set_edit(&path, &value, SetTarget::ExistingValue))
            .unwrap();
        assert!(out.contains("port: 1"), "out: {out}");
        let map = parse_map(&out);
        assert_eq!(
            map["items"],
            Value::Array(vec![Value::Integer(3), Value::Integer(4)])
        );
    }

    #[test]
    fn edit_set_seeds_missing_file_from_empty_or_commented_source() {
        // The create-file row: persist seeds from the template (possibly
        // all-commented) and sets into it; the comments survive.
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");

        let out = YamlAdapter
            .edit("", set_edit(&path, &value, SetTarget::MissingFile))
            .unwrap();
        let map = parse_map(&out);
        assert_eq!(
            map["database"].as_map().unwrap()["url"],
            Value::String("pg://x".into())
        );

        let seeded = "# My app config\n#port: 0\n";
        let out = YamlAdapter
            .edit(seeded, set_edit(&path, &value, SetTarget::MissingFile))
            .unwrap();
        assert!(out.starts_with("# My app config\n#port: 0\n"), "out: {out}");
        let map = parse_map(&out);
        assert_eq!(
            map["database"].as_map().unwrap()["url"],
            Value::String("pg://x".into())
        );
    }

    #[test]
    fn edit_set_inserts_before_end_of_document_marker() {
        // Marker-only documents: content must land INSIDE the document —
        // after `---`, before `...` — never after the end marker, where
        // it would sit outside the document and fail to parse.
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(1);

        let out = YamlAdapter
            .edit("...\n", set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert_eq!(out, "port: 1\n...\n");
        assert_eq!(parse_map(&out)["port"], Value::Integer(1));

        let out = YamlAdapter
            .edit("---\n...\n", set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert_eq!(out, "---\nport: 1\n...\n");
        assert_eq!(parse_map(&out)["port"], Value::Integer(1));

        // Comments around and between the markers survive in place.
        let out = YamlAdapter
            .edit(
                "# top\n---\n# body\n...\n",
                set_edit(&path, &value, SetTarget::MissingFile),
            )
            .unwrap();
        assert_eq!(out, "# top\n---\n# body\nport: 1\n...\n");
        assert_eq!(parse_map(&out)["port"], Value::Integer(1));
    }

    #[test]
    fn edit_set_into_syntactic_empty_map_stays_single_document() {
        // `{}` parses to an empty map but is NOT a blank document:
        // appending after it would produce a second root. The patch path
        // extends the flow mapping instead.
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(1);
        let out = YamlAdapter
            .edit("{}\n", set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert_eq!(parse_map(&out)["port"], Value::Integer(1));

        let out = YamlAdapter
            .edit(
                "# note\n{}\n",
                set_edit(&path, &value, SetTarget::MissingKey),
            )
            .unwrap();
        assert!(out.contains("# note"), "out: {out}");
        assert_eq!(parse_map(&out)["port"], Value::Integer(1));

        // A nested create into `{}` rides the same verified patch path.
        let deep = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        match YamlAdapter.edit("{}\n", set_edit(&deep, &value, SetTarget::MissingKey)) {
            // Either outcome is honest; a corrupt multi-doc file is not.
            Ok(out) => assert_eq!(
                parse_map(&out)["database"].as_map().unwrap()["url"],
                Value::String("pg://x".into())
            ),
            Err(FormatError::Unsupported(u)) => assert_eq!(u.operation, Operation::EditCreateKey),
            Err(other) => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn edit_set_path_conflict_is_typed_error() {
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let err = YamlAdapter
            .edit(
                "database: oops\n",
                set_edit(&path, &value, SetTarget::MissingKey),
            )
            .unwrap_err();
        match err {
            FormatError::Edit { message, .. } => {
                assert!(message.contains("path conflict"), "message: {message}");
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn edit_indexed_path_is_typed_error() {
        let path = ConfigPath::new().key("plugins").index(0).key("host");
        let value = Value::from("x");
        let err = YamlAdapter
            .edit("", set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap_err();
        match err {
            FormatError::Edit { message, .. } => {
                assert!(message.contains("[0]"), "{message}");
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn edit_unset_removes_key_and_missing_is_noop() {
        let source = "# keep me\nport: 8080\nhost: x\n";
        let path = ConfigPath::new().key("port");
        let out = YamlAdapter
            .edit(source, FileEdit::Unset { path: &path })
            .unwrap();
        assert_eq!(out, "# keep me\nhost: x\n");

        let missing = ConfigPath::new().key("nope").key("deep");
        let unchanged = YamlAdapter
            .edit(source, FileEdit::Unset { path: &missing })
            .unwrap();
        assert_eq!(unchanged, source);
    }

    #[test]
    fn edit_unset_sole_nested_child_preserves_parent_as_empty_map() {
        // Removing the last child must not leave `database:` — a null on
        // reparse. The emptied parent becomes an explicit `{}` (parity
        // with TOML, whose emptied tables keep their header).
        let path = ConfigPath::new().key("database").key("url");
        let out = YamlAdapter
            .edit(
                "# keep\ndatabase:\n  url: x\nport: 1\n",
                FileEdit::Unset { path: &path },
            )
            .unwrap();
        assert!(out.contains("# keep"), "out: {out}");
        let map = parse_map(&out);
        assert_eq!(map["database"], Value::Map(Map::new()));
        assert_eq!(map["port"], Value::Integer(1));

        // Deeper nesting: only the immediate parent empties.
        let path = ConfigPath::new().key("a").key("b").key("c");
        let out = YamlAdapter
            .edit(
                "a:\n  b:\n    c: 1\n  keep: 2\n",
                FileEdit::Unset { path: &path },
            )
            .unwrap();
        let a = parse_map(&out)["a"].as_map().unwrap().clone();
        assert_eq!(a["b"], Value::Map(Map::new()));
        assert_eq!(a["keep"], Value::Integer(2));

        // The parent emptied at every level above the leaf.
        let path = ConfigPath::new().key("only").key("child");
        let out = YamlAdapter
            .edit("only:\n  child: 1\n", FileEdit::Unset { path: &path })
            .unwrap();
        assert_eq!(parse_map(&out)["only"], Value::Map(Map::new()));
    }

    #[test]
    fn edit_unset_last_root_key_leaves_an_empty_document() {
        let path = ConfigPath::new().key("only");
        let out = YamlAdapter
            .edit("# note\nonly: 1\n", FileEdit::Unset { path: &path })
            .unwrap();
        assert!(out.contains("# note"), "out: {out}");
        assert_eq!(
            YamlAdapter.parse(&out).unwrap().value,
            Value::Map(Map::new())
        );
    }

    #[test]
    fn edit_datetime_value_lands_in_lexical_form() {
        let path = ConfigPath::new().key("launched");
        let value = Value::Datetime("2020-05-27T07:32:00Z".parse().unwrap());
        let out = YamlAdapter
            .edit("port: 1\n", set_edit(&path, &value, SetTarget::MissingKey))
            .unwrap();
        assert!(out.contains("launched: 2020-05-27T07:32:00Z"), "out: {out}");
    }

    // --- the known refusals (ADR-0002's YAML row) ---

    #[test]
    fn edit_refuses_flow_member_edits_instead_of_corrupting() {
        // yamlpatch rewrites flow-mapping members lossily (sibling keys
        // vanish); post-edit verification turns that into the typed
        // refusal instead of silent data loss.
        let source = "obj: {a: 1, b: 2}\n";
        let path = ConfigPath::new().key("obj").key("b");
        let value = Value::Integer(9);
        let result = YamlAdapter.edit(source, set_edit(&path, &value, SetTarget::ExistingValue));
        match result {
            // Either outcome is honest; corruption is not.
            Ok(out) => {
                let map = parse_map(&out);
                let obj = map["obj"].as_map().unwrap();
                assert_eq!(obj.get("a"), Some(&Value::Integer(1)));
                assert_eq!(obj.get("b"), Some(&Value::Integer(9)));
            }
            Err(FormatError::Unsupported(u)) => assert_eq!(u.operation, Operation::EditSet),
            Err(other) => panic!("expected Unsupported, got {other:?}"),
        }

        let err = YamlAdapter
            .edit(source, FileEdit::Unset { path: &path })
            .unwrap_err();
        match err {
            FormatError::Unsupported(u) => assert_eq!(u.operation, Operation::EditUnset),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
