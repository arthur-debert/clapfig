//! TOML format adapter — the baseline format behind the contract.
//!
//! This file is the ONLY place in the crate (outside `Cargo.toml`) that
//! touches the `toml` and `toml_edit` crates. It carries the behavior the
//! pipeline had before the value-model refactor, byte-identically:
//!
//! - [`parse`](TomlAdapter::parse): `toml` text → owned [`Value`] tree.
//! - [`serialize`](TomlAdapter::serialize): [`Value`] tree → TOML text.
//! - [`template`](TomlAdapter::template): the commented config template
//!   (doc comments, `# Allowed:` enum lines, commented placeholders for
//!   defaultless leaves) that `config gen` and file seeding emit.
//! - [`edit`](TomlAdapter::edit): comment-preserving `toml_edit` set/unset
//!   against existing source text.

use crate::runtime::{Field, LeafType, Schema};
use crate::value::{Map, Value};

use std::collections::BTreeMap;

use super::{
    ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, PathSegment, Span,
    UnsupportedByFormat,
};

/// The TOML format behind the adapter contract.
///
/// TOML is the baseline format: per ADR-0002's capability matrix it
/// declares every implemented operation with no known refusals, including
/// lossless comment-preserving edits. The one gap is
/// [`span_index`](TomlAdapter::span_index) — undeclared and refused typed
/// until the provenance epic builds the index.
pub struct TomlAdapter;

impl FormatAdapter for TomlAdapter {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
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
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
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
                span: e.span().map(|r| Span {
                    start: r.start,
                    end: r.end,
                }),
            }
        })?;
        Ok(toml_to_value(toml::Value::Table(table)))
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
        toml::to_string(&value_to_toml(value)).map_err(|e| FormatError::Serialize {
            format: "toml",
            message: e.to_string(),
        })
    }

    fn template(&self, schema: &Schema) -> Result<String, FormatError> {
        use std::fmt::Write;

        let mut out = String::new();
        for line in &schema.doc {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                out.push_str("#\n");
            } else {
                let _ = writeln!(out, "# {trimmed}");
            }
        }
        if !schema.doc.is_empty() {
            out.push('\n');
        }
        emit_schema(&mut out, schema, "");
        Ok(out)
    }

    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        let mut doc: toml_edit::DocumentMut =
            source
                .parse()
                .map_err(|e: toml_edit::TomlError| FormatError::Parse {
                    format: "toml",
                    message: e.message().to_string(),
                    span: e.span().map(|r| Span {
                        start: r.start,
                        end: r.end,
                    }),
                })?;
        match edit {
            FileEdit::Set { path, value, .. } => {
                let keys = key_segments(path);
                write_at_path(&mut doc, &keys, value)?;
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path);
                unset_at_path(&mut doc, &keys);
            }
        }
        Ok(doc.to_string())
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

/// Convert a parsed `toml::Value` into the owned model. Infallible: every
/// TOML construct maps directly onto the baseline (TOML *is* the baseline).
fn toml_to_value(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::Integer(i),
        toml::Value::Float(f) => Value::Float(f),
        toml::Value::Boolean(b) => Value::Boolean(b),
        // Identity: `toml::value::Datetime` IS the owned model's `Datetime`
        // (both are `toml_datetime`'s, same pinned version).
        toml::Value::Datetime(d) => Value::Datetime(d),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_value).collect()),
        toml::Value::Table(entries) => {
            let mut map = Map::new();
            for (k, v) in entries {
                map.insert(k, toml_to_value(v));
            }
            Value::Map(map)
        }
    }
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
/// walkers below.
fn key_segments(path: &super::ConfigPath) -> Vec<&str> {
    path.segments()
        .iter()
        .map(|PathSegment::Key(k)| k.as_str())
        .collect()
}

/// Walk `keys` through `doc`, creating intermediate tables when missing,
/// and assign `value` at the leaf. Errors when an existing intermediate is
/// a non-table scalar — `toml_edit`'s `IndexMut` would panic on that case,
/// and schema-time pre-checks only validate the schema, not the shape of
/// an existing on-disk file (`config set database.url x` with
/// `database = "string"` already in the file).
fn write_at_path(
    doc: &mut toml_edit::DocumentMut,
    keys: &[&str],
    value: &Value,
) -> Result<(), FormatError> {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let display_path = keys.join(".");
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for segment in parents {
        if current.get(segment).is_none() {
            if current.as_table_like_mut().is_none() {
                return Err(FormatError::Edit {
                    format: "toml",
                    message: format!(
                        "path conflict: existing file has a non-table value at the path before '{segment}' (setting '{display_path}')"
                    ),
                });
            }
            current[*segment] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let next = current
            .get_mut(*segment)
            .expect("just confirmed or inserted above");
        if next.as_table_like().is_none() {
            return Err(FormatError::Edit {
                format: "toml",
                message: format!(
                    "path conflict: existing file has a non-table value at '{segment}' (setting '{display_path}')"
                ),
            });
        }
        current = next;
    }
    if current.as_table_like_mut().is_none() {
        return Err(FormatError::Edit {
            format: "toml",
            message: format!(
                "path conflict: leaf parent is not a table (setting '{display_path}')"
            ),
        });
    }
    current[*leaf] = toml_edit::value(value_to_toml_edit(value));
    Ok(())
}

/// Remove the key at `keys`. A missing parent or leaf is a no-op — the
/// document is returned unchanged.
fn unset_at_path(doc: &mut toml_edit::DocumentMut, keys: &[&str]) {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for segment in parents {
        match current.get_mut(segment) {
            Some(item) => current = item,
            None => return, // parent doesn't exist, nothing to unset
        }
    }
    if let Some(table) = current.as_table_like_mut() {
        table.remove(leaf);
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

// --- template emission (relocated from `ops`; byte-identical output) -----

fn emit_schema(out: &mut String, schema: &Schema, prefix: &str) {
    use std::fmt::Write;

    // TOML rule: once a [section] header is emitted, every following key
    // belongs to that section until the next header. Emit local leaves
    // first, then sections — otherwise a sibling leaf declared after a
    // nested field in the schema would land inside the wrong section.
    for nf in &schema.fields {
        let Field::Leaf(leaf) = &nf.field else {
            continue;
        };
        for line in &leaf.doc {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                out.push_str("#\n");
            } else {
                let _ = writeln!(out, "# {trimmed}");
            }
        }
        if let LeafType::Enum { values } = &leaf.ty {
            let listed = values
                .iter()
                .map(format_inline_toml)
                .collect::<Vec<_>>()
                .join(" | ");
            let _ = writeln!(out, "# Allowed: {listed}");
        }
        if matches!(&leaf.ty, LeafType::Value) {
            let _ = writeln!(out, "# Accepts: any TOML value");
        }
        match &leaf.default {
            Some(value) => {
                let _ = writeln!(out, "{} = {}", nf.name, format_inline_toml(value));
            }
            None => {
                let hint = template_placeholder(&leaf.ty);
                let _ = writeln!(out, "#{} = {hint}", nf.name);
            }
        }
        out.push('\n');
    }

    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(_) => {} // already emitted above
            Field::Nested(child) => {
                let path = if prefix.is_empty() {
                    nf.name.clone()
                } else {
                    format!("{prefix}.{}", nf.name)
                };
                for line in &child.doc {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        out.push_str("#\n");
                    } else {
                        let _ = writeln!(out, "# {trimmed}");
                    }
                }
                let _ = writeln!(out, "[{path}]");
                emit_schema(out, child, &path);
            }
            Field::ArrayOf(child) => {
                let path = if prefix.is_empty() {
                    nf.name.clone()
                } else {
                    format!("{prefix}.{}", nf.name)
                };
                for line in &child.doc {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        out.push_str("#\n");
                    } else {
                        let _ = writeln!(out, "# {trimmed}");
                    }
                }
                // Array-of-objects is rendered commented — clapfig can't
                // know how many entries the user wants; show one example.
                let _ = writeln!(out, "#[[{path}]]");
                let mut buf = String::new();
                emit_schema(&mut buf, child, &path);
                for line in buf.lines() {
                    if line.is_empty() {
                        out.push('\n');
                    } else {
                        let _ = writeln!(out, "#{line}");
                    }
                }
            }
            Field::MapOf(child) => {
                let path = if prefix.is_empty() {
                    nf.name.clone()
                } else {
                    format!("{prefix}.{}", nf.name)
                };
                for line in &child.doc {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        out.push_str("#\n");
                    } else {
                        let _ = writeln!(out, "# {trimmed}");
                    }
                }
                // Map-of-objects: entry keys are user-supplied so emit a
                // commented example using a placeholder entry name.
                let _ = writeln!(out, "#[{path}.<key>]");
                let mut buf = String::new();
                emit_schema(&mut buf, child, &format!("{path}.<key>"));
                for line in buf.lines() {
                    if line.is_empty() {
                        out.push('\n');
                    } else {
                        let _ = writeln!(out, "#{line}");
                    }
                }
            }
        }
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

/// Single-word placeholder rendered in a commented-out template line for a
/// leaf without a default, hinting the expected value shape.
fn template_placeholder(ty: &LeafType) -> &'static str {
    match ty {
        LeafType::String => "\"\"",
        LeafType::Integer => "0",
        LeafType::Float => "0.0",
        LeafType::Bool => "false",
        LeafType::DateTime => "1970-01-01T00:00:00Z",
        LeafType::Array(_) => "[]",
        LeafType::Map(_) => "{}",
        LeafType::Enum { .. } => "\"\"",
        LeafType::Value => "\"\"",
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ConfigPath, SetTarget};
    use super::*;

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
#name = ""

# Any value.
# Accepts: any TOML value
#rule = ""

# Database settings
[db]
#url = ""

pool_size = 5

"#;
        assert_eq!(TomlAdapter.template(&schema).unwrap(), golden);
    }

    #[test]
    fn parse_scalars_and_containers() {
        let value = TomlAdapter
            .parse("s = \"x\"\ni = 3\nf = 1.5\nb = true\n[t]\nn = 1\narr = [1, 2]\n")
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
    fn parse_datetime_lands_in_owned_variant() {
        let value = TomlAdapter.parse("dt = 1979-05-27T07:32:00Z\n").unwrap();
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
    fn edit_set_hand_constructed_invalid_datetime_never_panics() {
        // The edit path writes `Display` output without revalidating:
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
        let value = TomlAdapter.parse(source).unwrap();
        let text = TomlAdapter.serialize(&value).unwrap();
        let reparsed = TomlAdapter.parse(&text).unwrap();
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
        let reparsed = TomlAdapter.parse(&out).unwrap();
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
