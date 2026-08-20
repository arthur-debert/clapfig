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

use crate::runtime::{Leaf, Schema};
use crate::value::{Map, Value};

use super::template::{
    TemplateRenderer, leaf_annotations, placeholder, push_comment_line, push_commented_block,
    walk_level,
};
use super::{FileEdit, FormatAdapter, FormatError, Operation, Parsed, PathSegment, Span};

/// The TOML format behind the adapter contract.
///
/// TOML is the baseline format: per ADR-0002's capability matrix it
/// declares every implemented operation with no known refusals, including
/// lossless comment-preserving edits. Span indexing rides on
/// [`parse`](TomlAdapter::parse) (ADR-0005); this workstream returns an
/// empty index as holding state.
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
        Ok(Parsed::from_value(toml_to_value(toml::Value::Table(table))))
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

    fn template(&self, schema: &Schema) -> Result<String, FormatError> {
        let mut out = String::new();
        for line in &schema.doc {
            push_comment_line(&mut out, "", line);
        }
        if !schema.doc.is_empty() {
            out.push('\n');
        }
        walk_level(&mut TomlTemplate, schema, &String::new(), &mut out)?;
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
                check_datetime_offsets(value)?;
                let keys = key_segments(path);
                super::edit::write_at_path(doc.as_item_mut(), &keys, value_to_toml_edit(value))?;
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path);
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
        .filter_map(|seg| match seg {
            PathSegment::Key(k) => Some(k.as_str()),
            // File edits address map keys via dotted persist paths;
            // index segments belong to span/origin trees.
            PathSegment::Index(_) => None,
        })
        .collect()
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
        leaf: &Leaf,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        for line in leaf_annotations(leaf, "TOML", &mut |v| Ok(format_inline_toml(v)))? {
            push_comment_line(out, "", &line);
        }
        match &leaf.default {
            Some(value) => {
                let _ = writeln!(out, "{name} = {}", format_inline_toml(value));
            }
            None => {
                let hint = placeholder(&leaf.ty, "\"\"", "1970-01-01T00:00:00Z");
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
        child: &Schema,
    ) -> Result<(), FormatError> {
        use std::fmt::Write;

        let path = section_path(prefix, name);
        for line in &child.doc {
            push_comment_line(out, "", line);
        }
        let _ = writeln!(out, "#[[{path}]]");
        let mut buf = String::new();
        walk_level(self, child, &path, &mut buf)?;
        push_commented_block(out, &buf);
        Ok(())
    }

    fn map_of(
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
        let _ = writeln!(out, "#[{path}.<key>]");
        let mut buf = String::new();
        walk_level(self, child, &format!("{path}.<key>"), &mut buf)?;
        push_commented_block(out, &buf);
        Ok(())
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
        assert_eq!(TomlAdapter.template(&schema).unwrap(), golden);
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
