//! TOML format adapter — the baseline format behind the contract.
//!
//! This file is the ONLY place in the crate (outside `Cargo.toml`) that
//! touches the `toml` and `toml_edit` crates. It carries the behavior the
//! pipeline had before the value-model refactor, byte-identically:
//!
//! - [`parse`](TomlAdapter::parse): one `toml_edit` document → owned
//!   [`Value`] tree **and** the path → span index (ADR-0005).
//! - [`serialize`](TomlAdapter::serialize): [`Value`] tree → TOML text.
//! - [`template`](TomlAdapter::template): the commented config template
//!   (doc comments, `# Allowed:` enum lines, commented placeholders for
//!   defaultless leaves) that `config gen` and file seeding emit.
//! - [`edit`](TomlAdapter::edit): comment-preserving `toml_edit` set/unset
//!   against existing source text.

use std::collections::BTreeMap;

use crate::runtime::{LeafType, Schema, Shape};
use crate::value::{Map, Value};

use super::template::{
    TemplateRenderer, leaf_annotations, placeholder, push_comment_line, push_commented_block,
    tagged_template_stub, walk_level, walk_root,
};
use super::{ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, Parsed, Span, SpanEntry};

/// The TOML format behind the adapter contract.
///
/// TOML is the baseline format: per ADR-0002's capability matrix it
/// declares every implemented operation with no known refusals, including
/// lossless comment-preserving edits. [`parse`](TomlAdapter::parse) walks
/// one `toml_edit` document (ImDocument, so spans survive) and returns
/// the value tree together with a complete path → span index (ADR-0005,
/// ADR-0006).
pub struct TomlAdapter;

impl FormatAdapter for TomlAdapter {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
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

    fn parse(&self, text: &str) -> Result<Parsed, FormatError> {
        // `ImDocument` retains byte spans; `DocumentMut` (used by edit)
        // clears them on construction. One walk produces both the Value
        // tree and the span index (ADR-0005).
        let doc = toml_edit::ImDocument::parse(text).map_err(|e: toml_edit::TomlError| {
            // `message()` is the bare description; some parser errors
            // express themselves through the span alone, so fall back to
            // the full rendering rather than an empty message.
            let message = match e.message() {
                "" => e.to_string(),
                m => m.to_string(),
            };
            FormatError::Parse {
                format: "toml",
                message,
                span: e.span().map(Span::from_range),
            }
        })?;
        let mut spans = BTreeMap::new();
        let value = Value::Map(table_to_map(doc.as_table(), &ConfigPath::new(), &mut spans));
        Ok(Parsed { value, spans })
    }

    fn serialize(&self, value: &Value) -> Result<String, FormatError> {
        let Value::Map(_) = value else {
            return Err(FormatError::Serialize {
                format: "toml",
                message: format!(
                    "TOML documents must be maps at the root, got {}",
                    value.type_str()
                ),
            });
        };
        check_datetime_offsets(value)?;
        toml::to_string(&value_to_toml(value)).map_err(|e| FormatError::Serialize {
            format: "toml",
            message: e.to_string(),
        })
    }

    fn template(&self, shape: &Shape) -> Result<String, FormatError> {
        let mut out = String::new();
        let doc = shape.field_doc();
        for line in doc {
            push_comment_line(&mut out, "", line);
        }
        if !doc.is_empty() {
            out.push('\n');
        }
        walk_root(&mut TomlTemplate, shape, &String::new(), &mut out)?;
        Ok(out)
    }

    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        let mut doc: toml_edit::DocumentMut =
            source
                .parse()
                .map_err(|e: toml_edit::TomlError| FormatError::Parse {
                    format: "toml",
                    message: e.message().to_string(),
                    span: e.span().map(Span::from_range),
                })?;
        match edit {
            FileEdit::Set { path, value, .. } => {
                check_datetime_offsets(value)?;
                let keys = key_segments(path)?;
                super::edit::write_at_path(doc.as_item_mut(), &keys, value_to_toml_edit(value))?;
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path)?;
                super::edit::unset_at_path(doc.as_item_mut(), &keys);
            }
        }
        Ok(doc.to_string())
    }
}

/// Refuse, as a typed serialize error, the one datetime the toml stack
/// cannot format: `toml`/`toml_edit` spell datetimes through
/// `toml_datetime`'s `Display` in code this adapter hands the value to,
/// and that impl overflows — a panic in overflow-checked builds — on
/// `Offset::Custom { minutes: i16::MIN }` (see the `value::datetime`
/// module docs). Every other hand-built garbage value keeps its existing
/// behavior: a typed error or a non-grammatical spelling from the toml
/// stack itself.
fn check_datetime_offsets(value: &Value) -> Result<(), FormatError> {
    match value {
        Value::Datetime(d) if crate::value::display_overflows(d) => Err(FormatError::Serialize {
            format: "toml",
            message: format!(
                "datetime offset of {} minutes overflows the TOML datetime formatter",
                i16::MIN
            ),
        }),
        Value::Array(items) => items.iter().try_for_each(check_datetime_offsets),
        Value::Map(map) => map.values().try_for_each(check_datetime_offsets),
        _ => Ok(()),
    }
}

/// Walk a `toml_edit` table into the owned model, recording a span-index
/// entry for every path. Infallible: every TOML construct maps directly
/// onto the baseline (TOML *is* the baseline).
fn table_to_map(
    table: &toml_edit::Table,
    path: &ConfigPath,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Map {
    table_like_to_map(table, path, spans)
}

fn table_like_to_map(
    table: &impl toml_edit::TableLike,
    path: &ConfigPath,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Map {
    let mut map = Map::new();
    for (k, item) in table.iter() {
        let Some(key) = table.key(k) else {
            unreachable!("iter() key must exist in the table");
        };
        let child = path.clone().key(k);
        let key_span = key.span().map(Span::from_range);
        let value = item_to_value(item, &child, key_span, spans);
        map.insert(k.to_string(), value);
    }
    map
}

fn item_to_value(
    item: &toml_edit::Item,
    path: &ConfigPath,
    key_span: Option<Span>,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Value {
    match item {
        toml_edit::Item::None => {
            record_span(spans, path.clone(), key_span, None);
            Value::Map(Map::new())
        }
        toml_edit::Item::Value(v) => {
            record_span(spans, path.clone(), key_span, v.span());
            edit_value_to_value(v, path, spans)
        }
        toml_edit::Item::Table(t) => {
            record_span(spans, path.clone(), key_span, t.span());
            Value::Map(table_to_map(t, path, spans))
        }
        toml_edit::Item::ArrayOfTables(a) => {
            record_span(spans, path.clone(), key_span, a.span());
            Value::Array(array_of_tables_to_values(a, path, spans))
        }
    }
}

fn edit_value_to_value(
    value: &toml_edit::Value,
    path: &ConfigPath,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Value {
    match value {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::Integer(*i.value()),
        toml_edit::Value::Float(f) => Value::Float(*f.value()),
        toml_edit::Value::Boolean(b) => Value::Boolean(*b.value()),
        // Identity: `toml_edit::Datetime` IS the owned model's `Datetime`
        // (both are `toml_datetime`'s, same pinned version).
        toml_edit::Value::Datetime(d) => Value::Datetime(*d.value()),
        toml_edit::Value::Array(a) => Value::Array(array_to_values(a, path, spans)),
        toml_edit::Value::InlineTable(t) => Value::Map(table_like_to_map(t, path, spans)),
    }
}

fn array_to_values(
    array: &toml_edit::Array,
    path: &ConfigPath,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Vec<Value> {
    array
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let child = path.clone().index(i);
            record_span(spans, child.clone(), None, v.span());
            edit_value_to_value(v, &child, spans)
        })
        .collect()
}

fn array_of_tables_to_values(
    array: &toml_edit::ArrayOfTables,
    path: &ConfigPath,
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
) -> Vec<Value> {
    array
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let child = path.clone().index(i);
            record_span(spans, child.clone(), None, t.span());
            Value::Map(table_to_map(t, &child, spans))
        })
        .collect()
}

fn record_span(
    spans: &mut BTreeMap<ConfigPath, SpanEntry>,
    path: ConfigPath,
    key: Option<Span>,
    value: Option<std::ops::Range<usize>>,
) {
    spans.insert(
        path,
        SpanEntry {
            key,
            // Implicit dotted/header parent tables may lack a value span;
            // fall back to the key token so every path in the tree has an
            // entry (ADR-0005: empty/partial indexes are not legal).
            value: value
                .map(Span::from_range)
                .or(key)
                .unwrap_or(Span { start: 0, end: 0 }),
        },
    );
}

/// Convert an owned [`Value`] into a `toml::Value` for serialization.
/// Infallible for the same reason as [`toml_to_value`].
fn value_to_toml(value: &Value) -> toml::Value {
    match value {
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Integer(i) => toml::Value::Integer(*i),
        Value::Float(f) => toml::Value::Float(*f),
        Value::Boolean(b) => toml::Value::Boolean(*b),
        // Identity (see `toml_to_value`). No reparse, so a hand-constructed
        // non-grammatical datetime (the component fields are public) cannot
        // panic here; `toml::to_string` rejects it as a serialize error.
        Value::Datetime(d) => toml::Value::Datetime(*d),
        Value::Array(items) => toml::Value::Array(items.iter().map(value_to_toml).collect()),
        Value::Map(map) => {
            let mut table = toml::Table::new();
            for (k, v) in map {
                table.insert(k.clone(), value_to_toml(v));
            }
            toml::Value::Table(table)
        }
    }
}

/// The key segments of a [`ConfigPath`](super::ConfigPath), for the edit
/// walkers below. Index segments refuse rather than silently retargeting
/// the edit.
fn key_segments(path: &super::ConfigPath) -> Result<Vec<&str>, FormatError> {
    super::edit::map_key_segments(path, "toml")
}

/// The TOML document tree behind the shared edit walkers
/// (`format::edit`). `toml_edit`'s `IndexMut` would panic where the
/// walkers' conflict checks refuse typed instead.
impl super::edit::EditDoc for toml_edit::Item {
    type Value = toml_edit::Value;

    const FORMAT: &'static str = "toml";
    const CONTAINER: &'static str = "table";
    const CONTAINER_WITH_ARTICLE: &'static str = "a table";
    const SOURCE: &'static str = "file";

    fn is_container(&self) -> bool {
        self.as_table_like().is_some()
    }

    fn has_child(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    fn child_mut(&mut self, key: &str) -> Option<&mut Self> {
        self.get_mut(key)
    }

    fn insert_container(&mut self, key: &str) {
        self[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    fn insert_value(&mut self, key: &str, value: toml_edit::Value) {
        self[key] = toml_edit::value(value);
    }

    fn remove_key(&mut self, key: &str) -> bool {
        self.as_table_like_mut()
            .is_some_and(|table| table.remove(key).is_some())
    }
}

/// Convert an owned [`Value`] into a `toml_edit::Value` for edits.
fn value_to_toml_edit(value: &Value) -> toml_edit::Value {
    match value {
        Value::String(s) => s.as_str().into(),
        Value::Integer(i) => (*i).into(),
        Value::Float(f) => (*f).into(),
        Value::Boolean(b) => (*b).into(),
        // Identity (see `toml_to_value`): `toml_edit::Datetime` is the same
        // `toml_datetime` type, so no reparse and no panic path.
        Value::Datetime(d) => (*d).into(),
        Value::Array(items) => {
            let arr: toml_edit::Array = items.iter().map(value_to_toml_edit).collect();
            arr.into()
        }
        Value::Map(map) => {
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in map {
                inline.insert(k, value_to_toml_edit(v));
            }
            inline.into()
        }
    }
}

// --- template emission (shared traversal in `format::template`) ----------

/// The TOML template renderer: `key = value` lines, dotted `[section]`
/// headers, commented `#[[path]]` / `#[path.<key>]` example blocks. The
/// context is the dotted section path (empty at the root).
struct TomlTemplate;

/// The dotted section path of `name` under `prefix`.
fn section_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

impl TemplateRenderer for TomlTemplate {
    type Ctx = String;
    type Out = String;

    // TOML rule: once a [section] header is emitted, every following key
    // belongs to that section until the next header — a sibling leaf
    // declared after a nested field would land inside the wrong section.
    const LEAVES_FIRST: bool = true;

    fn leaf(
        &mut self,
        out: &mut String,
        _prefix: &String,
        name: &str,
        field: super::template::ValueView<'_>,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        for line in leaf_annotations(field, "TOML", &mut |v| Ok(format_inline_toml(v)))? {
            push_comment_line(out, "", &line);
        }
        match field.default {
            Some(value) => {
                let _ = writeln!(out, "{name} = {}", format_inline_toml(value));
            }
            None => {
                let hint = placeholder(field.shape, "\"\"", "1970-01-01T00:00:00Z");
                let _ = writeln!(out, "#{name} = {hint}");
            }
        }
        out.push('\n');
        Ok(())
    }

    fn nested(
        &mut self,
        out: &mut String,
        prefix: &String,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        let path = section_path(prefix, name);
        for line in &child.doc {
            push_comment_line(out, "", line);
        }
        let _ = writeln!(out, "[{path}]");
        walk_level(self, child, &path, out)
    }

    fn array_of(
        &mut self,
        out: &mut String,
        prefix: &String,
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        // Nested anonymous arrays (`Array<Array<…>>`) have no `[[path]]`
        // spelling — a second header is another entry of the same array,
        // not a nested array. Emit a commented inline assignment that
        // keeps every layer. `item` is the array's item, so an Array
        // here is already a nested array.
        if matches!(item, Shape::Array(_)) || has_array_in_array(item) {
            emit_object_doc(out, item);
            let example = Value::Array(vec![example_shape_value(item)]);
            let _ = writeln!(out, "#{name} = {}", value_to_toml_edit(&example));
            return Ok(());
        }
        let path = section_path(prefix, name);
        emit_object_doc(out, item);
        let _ = writeln!(out, "#[[{path}]]");
        let mut buf = String::new();
        emit_toml_item(self, &mut buf, &path, item)?;
        push_commented_block(out, &buf);
        Ok(())
    }

    fn map_of(
        &mut self,
        out: &mut String,
        prefix: &String,
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        if has_array_in_array(item) {
            emit_object_doc(out, item);
            let mut entry = Map::new();
            entry.insert("<key>".into(), example_shape_value(item));
            let _ = writeln!(out, "#{name} = {}", value_to_toml_edit(&Value::Map(entry)));
            return Ok(());
        }
        let path = section_path(prefix, name);
        emit_object_doc(out, item);
        let entry = format!("{path}.<key>");
        let (header, inner) = match item {
            Shape::Array(array) => (format!("#[[{entry}]]"), array.item.as_ref()),
            other => (format!("#[{entry}]"), other),
        };
        let _ = writeln!(out, "{header}");
        let mut buf = String::new();
        emit_toml_item(self, &mut buf, &entry, inner)?;
        push_commented_block(out, &buf);
        Ok(())
    }

    fn root_map(
        &mut self,
        out: &mut String,
        _prefix: &String,
        item: &Shape,
        _doc: &[String],
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        // Value-shaped items are assignments, not tables: a leaf or
        // array/map of leaves has no `[<key>]` spelling, and routing
        // them through `emit_toml_item` hits its leaf `unreachable!`.
        if item.is_value_field() {
            let field = super::template::ValueView::from_shape(item);
            for line in leaf_annotations(field, "TOML", &mut |v| Ok(format_inline_toml(v)))? {
                push_comment_line(out, "", &line);
            }
            let example = match field.default {
                Some(value) => format_inline_toml(value),
                None => placeholder(field.shape, "\"\"", "1970-01-01T00:00:00Z").to_string(),
            };
            let _ = writeln!(out, "#<key> = {example}");
            return Ok(());
        }
        // Nested anonymous arrays have no `[[<key>]]` spelling — the
        // same inline fallback field-position `map_of` uses.
        if has_array_in_array(item) {
            emit_object_doc(out, item);
            let _ = writeln!(
                out,
                "#<key> = {}",
                value_to_toml_edit(&example_shape_value(item))
            );
            return Ok(());
        }
        emit_object_doc(out, item);
        let entry = "<key>";
        let (header, inner) = match item {
            Shape::Array(array) => (format!("#[[{entry}]]"), array.item.as_ref()),
            other => (format!("#[{entry}]"), other),
        };
        let _ = writeln!(out, "{header}");
        let mut buf = String::new();
        emit_toml_item(self, &mut buf, entry, inner)?;
        push_commented_block(out, &buf);
        Ok(())
    }
}

fn emit_object_doc(out: &mut String, item: &Shape) {
    if let Shape::Object(child) = item {
        for line in &child.doc {
            push_comment_line(out, "", line);
        }
    }
}

/// True when `shape` is an Array or contains an Array nested directly
/// inside another Array (anonymous nested arrays). Those cannot be
/// spelled as repeated `[[path]]` headers.
fn has_array_in_array(shape: &Shape) -> bool {
    match shape {
        Shape::Array(array) => {
            matches!(array.item.as_ref(), Shape::Array(_)) || has_array_in_array(&array.item)
        }
        Shape::Map(map) => has_array_in_array(&map.item),
        Shape::Leaf(_) | Shape::Object(_) | Shape::Tagged(_) => false,
    }
}

/// One-entry example value for a shape, used when a template must keep
/// nested array layers in inline TOML. Defaults win; otherwise a
/// placeholder (leaf) or a one-entry nested example (container).
fn example_shape_value(shape: &Shape) -> Value {
    match shape {
        Shape::Leaf(leaf) => leaf
            .default
            .clone()
            .unwrap_or_else(|| leaf_placeholder(&leaf.ty)),
        Shape::Object(schema) => {
            let mut map = Map::new();
            for nf in &schema.fields {
                map.insert(nf.name.clone(), example_shape_value(&nf.field));
            }
            Value::Map(map)
        }
        Shape::Array(array) => array
            .default
            .clone()
            .unwrap_or_else(|| Value::Array(vec![example_shape_value(&array.item)])),
        Shape::Map(map) => map.default.clone().unwrap_or_else(|| {
            let mut entry = Map::new();
            entry.insert("<key>".into(), example_shape_value(&map.item));
            Value::Map(entry)
        }),
        Shape::Tagged(_) => tagged_template_stub(),
    }
}

fn leaf_placeholder(ty: &LeafType) -> Value {
    match ty {
        LeafType::String | LeafType::Enum { .. } | LeafType::Value => Value::String(String::new()),
        LeafType::Integer { .. } => Value::Integer(0),
        LeafType::Float => Value::Float(0.0),
        LeafType::Bool => Value::Boolean(false),
        LeafType::DateTime => Value::Datetime(
            "1970-01-01T00:00:00Z"
                .parse()
                .expect("epoch datetime placeholder is valid"),
        ),
    }
}

/// Render nested item content under `path`. The caller already emitted
/// the field's own `[[path]]` / `[path.<key>]` header. Nested Maps add
/// `[path.<key>]`; a keyed Array (map of arrays of objects) adds
/// `[[path]]`. Nested anonymous arrays are inlined at the field, not here.
fn emit_toml_item(
    renderer: &mut TomlTemplate,
    out: &mut String,
    path: &str,
    item: &Shape,
) -> Result<(), FormatError> {
    use std::fmt::Write;

    match item {
        Shape::Object(child) => walk_level(renderer, child, &path.to_string(), out),
        Shape::Map(map) => {
            let entry = format!("{path}.<key>");
            match map.item.as_ref() {
                Shape::Array(array) if !has_array_in_array(&array.item) => {
                    let _ = writeln!(out, "[[{entry}]]");
                    emit_toml_item(renderer, out, &entry, &array.item)
                }
                Shape::Array(_) => {
                    let _ = writeln!(
                        out,
                        "<key> = {}",
                        value_to_toml_edit(&example_shape_value(&map.item))
                    );
                    Ok(())
                }
                other => {
                    let _ = writeln!(out, "[{entry}]");
                    emit_toml_item(renderer, out, &entry, other)
                }
            }
        }
        Shape::Array(array) => {
            let _ = writeln!(out, "[[{path}]]");
            emit_toml_item(renderer, out, path, &array.item)
        }
        Shape::Tagged(_) => tagged_template_stub(),
        Shape::Leaf(_) => unreachable!("value-field containers are emitted as leaves"),
    }
}

/// Format an owned [`Value`] as it would appear inline in a TOML file (no
/// surrounding whitespace, no trailing newline).
fn format_inline_toml(value: &Value) -> String {
    // toml::to_string handles inline encoding for primitives correctly;
    // for arrays/tables it produces a TOML fragment we trim.
    toml::to_string(&toml::Value::Table({
        let mut t = toml::Table::new();
        t.insert("v".into(), value_to_toml(value));
        t
    }))
    .map(|s| {
        // Output looks like `v = <inline>\n`. Strip the `v = ` prefix.
        let trimmed = s.trim_end();
        trimmed
            .strip_prefix("v = ")
            .map(|s| s.to_string())
            .unwrap_or_else(|| trimmed.to_string())
    })
    .unwrap_or_else(|_| format!("{value:?}"))
}

#[cfg(test)]
mod tests {
    use super::super::SetTarget;
    use super::*;

    fn collect_value_paths(value: &Value, path: ConfigPath, out: &mut Vec<ConfigPath>) {
        match value {
            Value::Map(map) => {
                for (k, v) in map {
                    let child = path.clone().key(k.clone());
                    out.push(child.clone());
                    collect_value_paths(v, child, out);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    let child = path.clone().index(i);
                    out.push(child.clone());
                    collect_value_paths(v, child, out);
                }
            }
            _ => {}
        }
    }

    fn source_slice(source: &str, span: Span) -> &str {
        &source[span.start..span.end]
    }

    #[test]
    fn template_matches_byte_identical_golden() {
        // Regression lock for the pipeline swap: relocating template
        // generation behind the adapter must not change a single byte of
        // the generated TOML template.
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
host = "localhost"

# Port number
port = 8080

# Log verbosity
# Allowed: "debug" | "info"
level = "info"

# Required name.
# Required.
#name = ""

# Any value.
# Accepts: any TOML value
# Required.
#rule = ""

# Database settings
[db]
#url = ""

pool_size = 5

"#;
        assert_eq!(
            TomlAdapter
                .template(&Shape::Object(schema.clone()))
                .unwrap(),
            golden
        );
    }

    #[test]
    fn template_recurses_through_nested_containers() {
        use crate::runtime::{Field, Schema as RtSchema};
        let item = RtSchema::object("Item").field("timeout", Field::integer().default(30i64));
        let schema = RtSchema::object("App")
            .field("groups", Field::array_of_type(Field::map_of(item.clone())))
            .field("batches", Field::map_of(Field::array_of_type(item)))
            .build();
        let text = TomlAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        assert!(
            text.contains("#[[groups]]"),
            "array-of-map emits array-of-tables header: {text}"
        );
        assert!(
            text.contains("#[groups.<key>]"),
            "array-of-map emits nested map header: {text}"
        );
        assert!(
            text.contains("#timeout = 30"),
            "nested object defaults stay in the commented example: {text}"
        );
        assert!(
            text.contains("#[[batches.<key>]]"),
            "map-of-array emits array-of-tables under the map key: {text}"
        );
    }

    fn uncommented_toml_assignments(template: &str) -> String {
        template
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix('#')
                    .map(str::trim_start)
                    .filter(|rest| rest.contains('=') && !rest.starts_with('['))
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn timeout_item() -> crate::runtime::Schema {
        use crate::runtime::{Field, Schema as RtSchema};
        RtSchema::object("Item")
            .field("timeout", Field::integer().default(30i64))
            .build()
    }

    #[test]
    fn template_nested_arrays_keep_every_layer_inline() {
        use crate::runtime::{Field, Schema as RtSchema};
        let item = timeout_item();
        let schema = RtSchema::object("App")
            .field(
                "matrix",
                Field::array_of_type(Field::array_of_type(item.clone())),
            )
            .field(
                "cube",
                Field::array_of_type(Field::array_of_type(Field::array_of_type(item.clone()))),
            )
            .field(
                "batches",
                Field::map_of(Field::array_of_type(Field::array_of_type(item))),
            )
            .build();
        let text = TomlAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        assert!(
            !text.contains("#[[matrix]]"),
            "must not flatten Array<Array<Object>> to one [[header]]: {text}"
        );
        assert!(
            !text.contains("#[[cube]]"),
            "must not flatten Array<Array<Array<Object>>> to one [[header]]: {text}"
        );
        let parsed = TomlAdapter
            .parse(&uncommented_toml_assignments(&text))
            .unwrap()
            .value;
        let map = parsed.as_map().unwrap();
        let timeout = Value::Map({
            let mut m = Map::new();
            m.insert("timeout".into(), Value::Integer(30));
            m
        });
        assert_eq!(
            &map["matrix"],
            &Value::Array(vec![Value::Array(vec![timeout.clone()])]),
            "uncommented Array<Array<Object>> example must keep both array layers: {text}"
        );
        assert_eq!(
            &map["cube"],
            &Value::Array(vec![Value::Array(vec![Value::Array(vec![
                timeout.clone()
            ])])]),
            "uncommented Array<Array<Array<Object>>> example must keep three array layers: {text}"
        );
        let batches = map["batches"].as_map().unwrap();
        assert_eq!(
            batches.get("<key>").or_else(|| batches.values().next()),
            Some(&Value::Array(vec![Value::Array(vec![timeout])])),
            "uncommented Map<Array<Array<Object>>> example must keep both array layers: {text}"
        );
    }

    #[test]
    fn template_root_map_covers_leaf_array_and_nested_map_items() {
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let object = RtSchema::object("Site")
            .field("host", Field::string().default("localhost"))
            .field("port", Field::integer().default(8080i64));

        let leaf = TomlAdapter
            .template(&Shape::from(Shape::map("values", Field::string())))
            .unwrap();
        assert!(
            leaf.contains("#<key> = \"\""),
            "root map of leaves is a commented assignment, not a table: {leaf}"
        );
        assert!(
            !leaf.contains("[<key>]") && !leaf.contains("[[<key>]]"),
            "value-shaped root map must not emit a table header: {leaf}"
        );

        let array_of_leaves = TomlAdapter
            .template(&Shape::from(Shape::map(
                "values",
                Field::array_of_type(LeafType::String),
            )))
            .unwrap();
        assert!(
            array_of_leaves.contains("#<key> = []"),
            "root map of scalar arrays is a commented assignment: {array_of_leaves}"
        );

        let array_of_objects = TomlAdapter
            .template(&Shape::from(Shape::map(
                "sites",
                Shape::array("sites", object.clone()),
            )))
            .unwrap();
        assert!(
            array_of_objects.contains("#[[<key>]]"),
            "root map of object arrays keeps array-of-tables: {array_of_objects}"
        );
        assert!(
            array_of_objects.contains("#host = \"localhost\""),
            "object-array item defaults stay in the example: {array_of_objects}"
        );

        let nested_map = TomlAdapter
            .template(&Shape::from(Shape::map(
                "groups",
                Shape::map("inner", object),
            )))
            .unwrap();
        assert!(
            nested_map.contains("#[<key>]"),
            "root map of maps keeps a table header: {nested_map}"
        );
        assert!(
            nested_map.contains("#[<key>.<key>]"),
            "nested map item adds an inner table header: {nested_map}"
        );
    }

    #[test]
    fn parse_scalars_and_containers() {
        let value = TomlAdapter
            .parse("s = \"x\"\ni = 3\nf = 1.5\nb = true\n[t]\nn = 1\narr = [1, 2]\n")
            .unwrap()
            .value;
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
    fn parse_fills_span_index_for_every_path() {
        let source = r#"s = "x"
i = 3
arr = [1, 2]

[t]
n = 1
"#;
        let parsed = TomlAdapter.parse(source).unwrap();
        let mut paths = Vec::new();
        collect_value_paths(&parsed.value, ConfigPath::new(), &mut paths);
        for path in &paths {
            assert!(
                parsed.spans.contains_key(path),
                "missing span for path {path}"
            );
        }
        assert_eq!(parsed.spans.len(), paths.len());
    }

    #[test]
    fn parse_span_index_key_none_on_array_elements() {
        let source = "arr = [1, { x = 2 }]\n";
        let parsed = TomlAdapter.parse(source).unwrap();
        let first = parsed
            .spans
            .get(&ConfigPath::new().key("arr").index(0))
            .expect("arr[0]");
        assert!(first.key.is_none(), "array elements have no key token");
        assert_eq!(source_slice(source, first.value), "1");
        let inline = parsed
            .spans
            .get(&ConfigPath::new().key("arr").index(1))
            .expect("arr[1]");
        assert!(inline.key.is_none());
        let x = parsed
            .spans
            .get(&ConfigPath::new().key("arr").index(1).key("x"))
            .expect("arr[1].x");
        assert_eq!(source_slice(source, x.key.expect("map key")), "x");
        assert_eq!(source_slice(source, x.value), "2");
    }

    #[test]
    fn parse_span_index_array_of_tables_entries_have_no_key() {
        let source = "[[servers]]\nhost = \"a\"\n[[servers]]\nhost = \"b\"\n";
        let parsed = TomlAdapter.parse(source).unwrap();
        let entry = parsed
            .spans
            .get(&ConfigPath::new().key("servers").index(1))
            .expect("servers[1]");
        assert!(
            entry.key.is_none(),
            "[[servers]] entries have no key token (ADR-0006)"
        );
        let host = parsed
            .spans
            .get(&ConfigPath::new().key("servers").index(1).key("host"))
            .expect("servers[1].host");
        assert_eq!(source_slice(source, host.key.expect("host key")), "host");
        assert_eq!(source_slice(source, host.value), "\"b\"");
    }

    #[test]
    fn parse_span_index_covers_inline_tables() {
        let source = "point = { x = 1, y = 2 }\n";
        let parsed = TomlAdapter.parse(source).unwrap();
        let x = parsed
            .spans
            .get(&ConfigPath::new().key("point").key("x"))
            .expect("point.x");
        assert_eq!(source_slice(source, x.key.expect("x key")), "x");
        assert_eq!(source_slice(source, x.value), "1");
    }

    #[test]
    fn parse_span_index_quoted_dotted_key_is_one_segment() {
        let source = "\"a.b\" = 1\n[a]\nb = 2\n";
        let parsed = TomlAdapter.parse(source).unwrap();
        let literal = parsed
            .spans
            .get(&ConfigPath::new().key("a.b"))
            .expect("literal a.b");
        assert_eq!(
            source_slice(source, literal.key.expect("quoted key")),
            "\"a.b\""
        );
        assert_eq!(source_slice(source, literal.value), "1");
        let nested = parsed
            .spans
            .get(&ConfigPath::new().key("a").key("b"))
            .expect("nested a.b");
        assert_eq!(source_slice(source, nested.key.expect("b key")), "b");
        assert_eq!(source_slice(source, nested.value), "2");
        assert_ne!(literal.key, nested.key);
    }

    #[test]
    fn parse_span_index_key_span_is_original_spelling() {
        let source = "pool-size = 5\n";
        let parsed = TomlAdapter.parse(source).unwrap();
        let entry = parsed
            .spans
            .get(&ConfigPath::new().key("pool-size"))
            .expect("pool-size");
        assert_eq!(
            source_slice(source, entry.key.expect("key span")),
            "pool-size"
        );
        assert_eq!(source_slice(source, entry.value), "5");
    }

    #[test]
    fn parse_datetime_lands_in_owned_variant() {
        let value = TomlAdapter
            .parse("dt = 1979-05-27T07:32:00Z\n")
            .unwrap()
            .value;
        let dt = value.as_map().unwrap()["dt"].as_datetime().unwrap();
        assert_eq!(dt.to_string(), "1979-05-27T07:32:00Z");
    }

    #[test]
    fn parse_error_carries_message_and_span() {
        let err = TomlAdapter.parse("key = ").unwrap_err();
        match err {
            FormatError::Parse {
                format,
                message,
                span,
            } => {
                assert_eq!(format, "toml");
                assert!(!message.is_empty());
                assert!(span.is_some(), "toml parse errors report a span");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn serialize_hand_constructed_invalid_datetime_is_typed_error() {
        // The component fields are public (`toml_datetime`'s types), so
        // safe code can assemble non-grammatical values. The adapter
        // converts by identity — no reparse, no panic path — and
        // `toml::to_string` rejects the invalid shape as a typed error.
        use crate::value::{Date, Datetime};
        let month13 = Datetime {
            date: Some(Date {
                year: 1979,
                month: 13,
                day: 1,
            }),
            time: None,
            offset: None,
        };
        let empty = Datetime {
            date: None,
            time: None,
            offset: None,
        };
        for dt in [month13, empty] {
            let mut map = Map::new();
            map.insert("dt".into(), Value::Datetime(dt));
            let err = TomlAdapter.serialize(&Value::Map(map)).unwrap_err();
            assert!(
                matches!(err, FormatError::Serialize { .. }),
                "expected typed serialize error for {dt:?}"
            );
        }
    }

    #[test]
    fn serialize_out_of_range_nanoseconds_never_panics() {
        // The garbage-in-garbage-out edge of the public-fields contract:
        // `Display` normalizes oversized nanoseconds into a (different)
        // valid spelling. The guarantee under test is no panic.
        use crate::value::{Datetime, Time};
        let dt = Datetime {
            date: None,
            time: Some(Time {
                hour: 7,
                minute: 32,
                second: 0,
                nanosecond: 1_999_999_999,
            }),
            offset: None,
        };
        let mut map = Map::new();
        map.insert("dt".into(), Value::Datetime(dt));
        let out = TomlAdapter.serialize(&Value::Map(map)).unwrap();
        assert_eq!(out, "dt = 07:32:00.199999999\n");
    }

    #[test]
    fn serialize_and_edit_refuse_offset_the_toml_stack_cannot_format() {
        // The one hand-built datetime the toml stack cannot even spell
        // as garbage: upstream `Display` overflows negating
        // `Offset::Custom { minutes: i16::MIN }` (a panic in
        // overflow-checked builds), so the adapter refuses it before
        // handing the value over (see `check_datetime_offsets`).
        use crate::value::{Date, Datetime, Offset, Time};
        let dt = Datetime {
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
        };

        let mut map = Map::new();
        map.insert("dt".into(), Value::Datetime(dt));
        let err = TomlAdapter.serialize(&Value::Map(map)).unwrap_err();
        assert!(matches!(err, FormatError::Serialize { .. }), "{err:?}");

        let path = ConfigPath::new().key("dt");
        let value = Value::Datetime(dt);
        let err = TomlAdapter
            .edit(
                "",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap_err();
        assert!(matches!(err, FormatError::Serialize { .. }), "{err:?}");
    }

    #[test]
    fn edit_set_hand_constructed_invalid_datetime_never_panics() {
        // The edit path writes `Display` output without revalidating
        // (beyond the one offset `check_datetime_offsets` refuses):
        // garbage in, garbage out — but never a panic (the conversion is
        // identity, no reparse).
        use crate::value::{Date, Datetime};
        let dt = Datetime {
            date: Some(Date {
                year: 1979,
                month: 13,
                day: 1,
            }),
            time: None,
            offset: None,
        };
        let path = ConfigPath::new().key("dt");
        let value = Value::Datetime(dt);
        let out = TomlAdapter
            .edit(
                "",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap();
        assert_eq!(out, "dt = 1979-13-01\n");
    }

    #[test]
    fn serialize_round_trips_parse() {
        let source = "b = true\ni = 3\ns = \"x\"\n\n[t]\nn = 1\n";
        let value = TomlAdapter.parse(source).unwrap().value;
        let text = TomlAdapter.serialize(&value).unwrap();
        let reparsed = TomlAdapter.parse(&text).unwrap().value;
        assert_eq!(value, reparsed);
    }

    #[test]
    fn serialize_rejects_non_map_root() {
        let err = TomlAdapter.serialize(&Value::Integer(1)).unwrap_err();
        assert!(matches!(err, FormatError::Serialize { .. }));
    }

    #[test]
    fn edit_set_preserves_comments() {
        let source = "# my note\nport = 8080\n";
        let path = ConfigPath::new().key("port");
        let value = Value::Integer(3000);
        let out = TomlAdapter
            .edit(
                source,
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::ExistingValue,
                },
            )
            .unwrap();
        assert!(out.contains("# my note"));
        assert!(out.contains("port = 3000"));
    }

    #[test]
    fn edit_set_creates_missing_path() {
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let out = TomlAdapter
            .edit(
                "",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
            .unwrap();
        let reparsed = TomlAdapter.parse(&out).unwrap().value;
        assert_eq!(
            reparsed.as_map().unwrap()["database"].as_map().unwrap()["url"],
            Value::String("pg://x".into())
        );
    }

    #[test]
    fn edit_set_path_conflict_is_typed_error() {
        let path = ConfigPath::new().key("database").key("url");
        let value = Value::from("pg://x");
        let err = TomlAdapter
            .edit(
                "database = \"oops\"\n",
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
    fn edit_indexed_path_is_typed_error() {
        let path = ConfigPath::new().key("plugins").index(0).key("host");
        let value = Value::from("x");
        let err = TomlAdapter
            .edit(
                "port = 1\n",
                FileEdit::Set {
                    path: &path,
                    value: &value,
                    target: SetTarget::MissingKey,
                },
            )
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
        let path = ConfigPath::new().key("port");
        let out = TomlAdapter
            .edit("port = 1\nhost = \"x\"\n", FileEdit::Unset { path: &path })
            .unwrap();
        assert!(!out.contains("port"));
        assert!(out.contains("host = \"x\""));

        let missing = ConfigPath::new().key("nope").key("deep");
        let unchanged = TomlAdapter
            .edit("port = 1\n", FileEdit::Unset { path: &missing })
            .unwrap();
        assert!(unchanged.contains("port = 1"));
    }
}
