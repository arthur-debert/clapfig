//! JSON format adapter — owned parse walk; `serde_json` for serialize/edit.
//!
//! This file is the ONLY place in the crate that gives `serde_json` a
//! *format* role (the crate also serves JSON Schema export, which is not a
//! config format). Parse is a clapfig-owned walk (ADR-0007): `serde_json`
//! has no byte offsets, and a second locate-keys pass is the desync
//! ADR-0005 forbids. `serde_json` stays for serialize and edit
//! (order-preserving pretty-print, comments-as-data). The file implements
//! the full ADR-0002 matrix row set for JSON, with no known
//! operation-level refusals:
//!
//! - [`parse`](JsonAdapter::parse): one walk over the source emits the
//!   owned [`Value`] tree and the path → [`SpanEntry`](super::SpanEntry)
//!   index (ADR-0005, ADR-0006). The `"//"` comment-key convention is
//!   applied in that same walk: every `//`-prefixed member is format
//!   syntax owned by this adapter and is stripped before the core
//!   [`Value`] tree exists — exactly as TOML's `#` comments never reach
//!   the tree — and is absent from the span index. The stripped
//!   namespace is reserved at any nesting depth; a `//`-prefixed member
//!   can never be a configuration key.
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
//! (the owned walk classifies a number as an integer iff its lexeme has
//! no fraction and no exponent). A float literal overflowing `f64` is
//! likewise a typed error. Those mapping errors carry the offending
//! token's byte span. Whitespace-only source parses as the empty map
//! (an empty config file is "no config", matching TOML's empty
//! document).

use std::collections::BTreeMap;

use serde_json::{Map as JsonMap, Value as Json};

use crate::runtime::{Schema, Shape};
use crate::value::{Map, Value};

use super::template::{
    TemplateRenderer, doc_lines, example_leaf_value, leaf_annotations,
    tagged_variant_example_schema, walk_level, walk_root,
};
use super::{
    ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, Parsed, PathSegment, Span,
    SpanEntry, walk_label,
};

/// The canonical format name used in error messages.
const FORMAT: &str = "json";

/// The reserved comment-key namespace (ADR-0002): any object member whose
/// key starts with this prefix is a comment, at any nesting depth.
const COMMENT_PREFIX: &str = "//";

/// The JSON format behind the adapter contract.
///
/// Declares every ADR-0002 matrix row (JSON has no refusal rows).
/// [`parse`](JsonAdapter::parse) is the owned walk that fills the span
/// index in the same pass as the [`Value`] tree (ADR-0005, ADR-0007).
/// See the [module docs](self) for the comment-key convention and the
/// baseline mapping rules this adapter applies.
pub struct JsonAdapter;

impl FormatAdapter for JsonAdapter {
    fn name(&self) -> &'static str {
        FORMAT
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
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
        // Encode the key as a JSON string rather than wrapping it in
        // literal quotes: runtime-schema key names may contain quotes,
        // backslashes, or control characters, and hand-rolled quoting
        // would render them misleadingly.
        let encoded = serde_json::to_string(key).expect("serializing a string to JSON cannot fail");
        format!("{encoded}: {value}")
    }

    fn display_comment(&self, line: &str) -> String {
        format!("// {line}")
    }

    fn parse(&self, text: &str) -> Result<Parsed, FormatError> {
        parse_document(text)
    }

    fn serialize(&self, value: &Value) -> Result<String, FormatError> {
        let mut path = Vec::new();
        let json = value_to_json(value, &mut path)?;
        Ok(render(&json))
    }

    fn template(&self, shape: &Shape) -> Result<String, FormatError> {
        let mut object = JsonMap::new();
        walk_root(&mut JsonTemplate, shape, &(), &mut object)?;
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
                let mut value_path: Vec<PathSegment> = path.segments().to_vec();
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

// --- owned parse walk (ADR-0007) -----------------------------------------

/// Matches `serde_json`'s default recursion ceiling so a hostile nest
/// is a typed parse error, not a stack overflow.
const MAX_DEPTH: usize = 128;

/// One-pass JSON parse: emit the [`Value`] tree and the span index
/// together, applying the baseline mapping rules (strip `//` keys,
/// refuse null, integer/float range) as the tokens are consumed.
fn parse_document(text: &str) -> Result<Parsed, FormatError> {
    // An empty (or whitespace-only) file is "no config", matching
    // TOML's empty document — not a JSON syntax error. There is no
    // source value to locate, so the index stays empty.
    if text.trim().is_empty() {
        return Ok(Parsed::from_value(Value::Map(Map::new())));
    }
    let mut parser = Parser::new(text);
    let value = parser.parse_value(None)?;
    parser.skip_ws();
    if parser.pos < text.len() {
        return parser.fail("unexpected trailing content after JSON value");
    }
    Ok(Parsed {
        value,
        spans: parser.spans,
    })
}

/// Byte-offset walker over one JSON document.
struct Parser<'a> {
    text: &'a str,
    pos: usize,
    depth: usize,
    path: Vec<PathSegment>,
    spans: BTreeMap<ConfigPath, SpanEntry>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            pos: 0,
            depth: 0,
            path: Vec::new(),
            spans: BTreeMap::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn here(&self) -> Span {
        if self.pos >= self.text.len() {
            return Span {
                start: self.text.len(),
                end: self.text.len(),
            };
        }
        Span {
            start: self.pos,
            end: self.text.ceil_char_boundary(self.pos + 1),
        }
    }

    fn parse_error(&self, message: String, span: Span) -> FormatError {
        FormatError::Parse {
            format: FORMAT,
            message,
            span: Some(span),
        }
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, FormatError> {
        Err(self.parse_error(message.into(), self.here()))
    }

    fn expect(&mut self, wanted: u8) -> Result<(), FormatError> {
        match self.peek() {
            Some(b) if b == wanted => {
                self.pos += 1;
                Ok(())
            }
            _ => self.fail(format!("expected '{}'", wanted as char)),
        }
    }

    fn enter(&mut self) -> Result<(), FormatError> {
        if self.depth >= MAX_DEPTH {
            return self.fail("nesting exceeds the JSON parse limit");
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn record(&mut self, key: Option<Span>, value: Span) {
        self.spans.insert(
            ConfigPath::from(self.path.clone()),
            SpanEntry { key, value },
        );
    }

    /// Drop the current path and every descendant. Used when a later
    /// duplicate key replaces an earlier member so the index cannot
    /// retain paths the returned tree no longer has.
    fn drop_current_prefix(&mut self) {
        let prefix = self.path.as_slice();
        self.spans.retain(|path, _| {
            let segs = path.segments();
            !(segs.len() >= prefix.len() && segs[..prefix.len()] == *prefix)
        });
    }

    /// Parse one JSON value at the current path, apply baseline rules,
    /// and record its span. `key` is the member-key token (quoted) or
    /// `None` on the document root and on array elements (ADR-0006).
    fn parse_value(&mut self, key: Option<Span>) -> Result<Value, FormatError> {
        self.skip_ws();
        let start = self.pos;
        let value = match self.peek() {
            Some(b'{') => self.parse_object()?,
            Some(b'[') => self.parse_array()?,
            Some(b'"') => Value::String(self.parse_string()?.0),
            Some(b't') => {
                self.expect_literal("true")?;
                Value::Boolean(true)
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Value::Boolean(false)
            }
            Some(b'n') => {
                let span = self.expect_literal("null")?;
                return Err(self.parse_error(
                    format!(
                        "null at {} is not a configuration value: absence expresses unset — omit the key instead",
                        walk_label(&self.path)
                    ),
                    span,
                ));
            }
            Some(b'-' | b'0'..=b'9') => self.parse_number()?,
            Some(_) => return self.fail("expected a JSON value"),
            None => return self.fail("unexpected end of JSON input"),
        };
        self.record(
            key,
            Span {
                start,
                end: self.pos,
            },
        );
        Ok(value)
    }

    fn parse_object(&mut self) -> Result<Value, FormatError> {
        self.enter()?;
        self.expect(b'{')?;
        self.skip_ws();
        let mut map = Map::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.leave();
            return Ok(Value::Map(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return self.fail("expected a string key");
            }
            let (key, key_span) = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            if key.starts_with(COMMENT_PREFIX) {
                // Comment syntax, not configuration: consume the value
                // without baseline mapping and without indexing it.
                self.skip_json()?;
            } else {
                self.path.push(PathSegment::Key(key.clone()));
                if map.contains_key(&key) {
                    self.drop_current_prefix();
                }
                let converted = self.parse_value(Some(key_span))?;
                self.path.pop();
                map.insert(key, converted);
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.fail("expected ',' or '}' after object member"),
            }
        }
        self.leave();
        Ok(Value::Map(map))
    }

    fn parse_array(&mut self) -> Result<Value, FormatError> {
        self.enter()?;
        self.expect(b'[')?;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.leave();
            return Ok(Value::Array(items));
        }
        loop {
            let index = items.len();
            self.path.push(PathSegment::Index(index));
            let item = self.parse_value(None)?;
            self.path.pop();
            items.push(item);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.fail("expected ',' or ']' after array element"),
            }
        }
        self.leave();
        Ok(Value::Array(items))
    }

    /// Consume one JSON value without applying clapfig mapping rules.
    /// Used for `//`-prefixed comment members, whose payload may be
    /// null or an out-of-range integer and must not trip those errors.
    fn skip_json(&mut self) -> Result<(), FormatError> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.skip_object(),
            Some(b'[') => self.skip_array(),
            Some(b'"') => {
                self.parse_string()?;
                Ok(())
            }
            Some(b't') => {
                self.expect_literal("true")?;
                Ok(())
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Ok(())
            }
            Some(b'n') => {
                self.expect_literal("null")?;
                Ok(())
            }
            Some(b'-' | b'0'..=b'9') => {
                self.lex_number()?;
                Ok(())
            }
            Some(_) => self.fail("expected a JSON value"),
            None => self.fail("unexpected end of JSON input"),
        }
    }

    fn skip_object(&mut self) -> Result<(), FormatError> {
        self.enter()?;
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.leave();
            return Ok(());
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return self.fail("expected a string key");
            }
            self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_json()?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.fail("expected ',' or '}' after object member"),
            }
        }
        self.leave();
        Ok(())
    }

    fn skip_array(&mut self) -> Result<(), FormatError> {
        self.enter()?;
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.leave();
            return Ok(());
        }
        loop {
            self.skip_json()?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.fail("expected ',' or ']' after array element"),
            }
        }
        self.leave();
        Ok(())
    }

    fn expect_literal(&mut self, literal: &str) -> Result<Span, FormatError> {
        let start = self.pos;
        if !self.text[self.pos..].starts_with(literal) {
            return self.fail(format!("expected {literal}"));
        }
        self.pos += literal.len();
        if self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return self.fail(format!("expected {literal}"));
        }
        Ok(Span {
            start,
            end: self.pos,
        })
    }

    fn parse_string(&mut self) -> Result<(String, Span), FormatError> {
        let start = self.pos;
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.parse_error(
                    "unterminated string".into(),
                    Span {
                        start,
                        end: self.text.len(),
                    },
                ));
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok((
                        out,
                        Span {
                            start,
                            end: self.pos,
                        },
                    ));
                }
                b'\\' => self.parse_escape(&mut out)?,
                0x00..=0x1F => return self.fail("unescaped control character in string"),
                _ => {
                    let ch = self.text[self.pos..]
                        .chars()
                        .next()
                        .expect("peeked a byte so a char remains");
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_escape(&mut self, out: &mut String) -> Result<(), FormatError> {
        self.pos += 1;
        let Some(b) = self.peek() else {
            return self.fail("unterminated string escape");
        };
        self.pos += 1;
        match b {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => self.parse_unicode_escape(out)?,
            _ => return self.fail("invalid escape sequence"),
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16, FormatError> {
        let Some(hex) = self.text.get(self.pos..self.pos + 4) else {
            return self.fail("unterminated unicode escape");
        };
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self.fail("invalid unicode escape");
        }
        let unit = u16::from_str_radix(hex, 16).expect("four ASCII hex digits parse as a u16");
        self.pos += 4;
        Ok(unit)
    }

    fn parse_unicode_escape(&mut self, out: &mut String) -> Result<(), FormatError> {
        let unit = self.parse_hex4()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            if self.peek() != Some(b'\\') {
                return self.fail("unpaired UTF-16 surrogate");
            }
            self.pos += 1;
            if self.peek() != Some(b'u') {
                return self.fail("unpaired UTF-16 surrogate");
            }
            self.pos += 1;
            let low = self.parse_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return self.fail("unpaired UTF-16 surrogate");
            }
            let cp = 0x10000 + (u32::from(unit - 0xD800) << 10) + u32::from(low - 0xDC00);
            out.push(char::from_u32(cp).expect("surrogate pair decodes to a scalar"));
        } else if (0xDC00..=0xDFFF).contains(&unit) {
            return self.fail("unpaired UTF-16 surrogate");
        } else {
            out.push(char::from_u32(u32::from(unit)).expect("non-surrogate unit is a scalar"));
        }
        Ok(())
    }

    fn lex_number(&mut self) -> Result<(Span, bool), FormatError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    return self.fail("leading zeros are not allowed in JSON numbers");
                }
            }
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return self.fail("expected a JSON number"),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return self.fail("expected a digit after the decimal point");
            }
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return self.fail("expected a digit in the exponent");
            }
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        Ok((
            Span {
                start,
                end: self.pos,
            },
            is_float,
        ))
    }

    fn parse_number(&mut self) -> Result<Value, FormatError> {
        let (span, is_float) = self.lex_number()?;
        let lexeme = &self.text[span.start..span.end];
        if is_float {
            return match lexeme.parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(Value::Float(f)),
                // Overflow (e.g. 1e999) becomes infinity; underflow to
                // zero stays finite. Non-finite is a typed range error.
                _ => Err(self.parse_error(
                    format!(
                        "float {lexeme} at {} is out of range: the value model's floats are 64-bit (f64)",
                        walk_label(&self.path)
                    ),
                    span,
                )),
            };
        }
        match lexeme.parse::<i64>() {
            Ok(i) => Ok(Value::Integer(i)),
            Err(_) => Err(self.parse_error(
                format!(
                    "integer {lexeme} at {} is out of range: the value model's integers are 64-bit signed (i64)",
                    walk_label(&self.path)
                ),
                span,
            )),
        }
    }
}

/// Convert an owned [`Value`] into a `serde_json::Value`. The one
/// unrepresentable shape is a non-finite float (JSON has no literal for
/// it) — a typed error naming the offending path.
fn value_to_json(value: &Value, path: &mut Vec<PathSegment>) -> Result<Json, FormatError> {
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
                path.push(PathSegment::Index(i));
                let converted = value_to_json(item, path)?;
                path.pop();
                out.push(converted);
            }
            Json::Array(out)
        }
        Value::Map(map) => {
            let mut out = JsonMap::new();
            for (key, entry) in map {
                path.push(PathSegment::Key(key.clone()));
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
/// address a configuration key, and refusing index segments (file edits
/// address map keys).
fn key_segments(path: &ConfigPath) -> Result<Vec<&str>, FormatError> {
    let keys = super::edit::map_key_segments(path, FORMAT)?;
    if let Some(k) = keys.iter().copied().find(|k| k.starts_with(COMMENT_PREFIX)) {
        return Err(FormatError::Edit {
            format: FORMAT,
            message: reserved_key_message(&format!("'{k}'")),
        });
    }
    Ok(keys)
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
        field: super::template::ValueView<'_>,
    ) -> Result<(), FormatError> {
        let mut lines = leaf_annotations(field, "JSON", &mut |v| inline_json(v, name))?;
        match field.default {
            Some(default) => {
                if !lines.is_empty() {
                    out.insert(comment_key(name), comment_value(lines));
                }
                let mut path = vec![PathSegment::Key(name.to_string())];
                out.insert(name.to_string(), value_to_json(default, &mut path)?);
            }
            None => {
                // JSON cannot comment out a real key, so the assignment
                // snippet rides inside the comment — the counterpart of
                // TOML's `#name = ""` line.
                lines.push(assignment_snippet(name, placeholder_json(field.shape)));
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
        ctx: &(),
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError> {
        // Entry count is the user's call, so no real key is emitted (an
        // absent array-of resolves to the empty list); the comment carries
        // a one-entry example (one per tagged variant).
        let mut lines = doc_lines(item.field_doc());
        if let Shape::Tagged(tagged) = item {
            for variant in &tagged.variants {
                let example = Json::Array(vec![tagged_variant_json(self, ctx, tagged, variant)?]);
                lines.push(assignment_snippet(name, compact(&example)));
            }
        } else {
            let example = Json::Array(vec![example_value(item, name)?]);
            lines.push(assignment_snippet(name, compact(&example)));
        }
        out.insert(comment_key(name), comment_value(lines));
        Ok(())
    }

    fn map_of(
        &mut self,
        out: &mut Self::Out,
        ctx: &(),
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError> {
        // Entry keys are user-supplied, so no real key is emitted (an
        // absent map-of resolves to the empty map); the comment carries a
        // placeholder-keyed example (one per tagged variant).
        let mut lines = doc_lines(item.field_doc());
        if let Shape::Tagged(tagged) = item {
            for variant in &tagged.variants {
                let mut example = JsonMap::new();
                example.insert(
                    "<key>".to_string(),
                    tagged_variant_json(self, ctx, tagged, variant)?,
                );
                lines.push(assignment_snippet(name, compact(&Json::Object(example))));
            }
        } else if let Some(tagged) = tagged_array_item(item) {
            for variant in &tagged.variants {
                let mut example = JsonMap::new();
                example.insert(
                    "<key>".to_string(),
                    Json::Array(vec![tagged_variant_json(self, ctx, tagged, variant)?]),
                );
                lines.push(assignment_snippet(name, compact(&Json::Object(example))));
            }
        } else {
            let mut example = JsonMap::new();
            example.insert("<key>".to_string(), example_value(item, name)?);
            lines.push(assignment_snippet(name, compact(&Json::Object(example))));
        }
        out.insert(comment_key(name), comment_value(lines));
        Ok(())
    }

    fn root_map(
        &mut self,
        out: &mut Self::Out,
        ctx: &(),
        item: &Shape,
        doc: &[String],
    ) -> Result<(), FormatError> {
        // Root-map docs live on the MapShape (`doc`); item docs are the
        // entry example's own prose. The assignment value is the item
        // example directly — wrapping it in a second `<key>` object
        // would advertise a map level the schema does not accept.
        let mut lines = doc_lines(doc);
        lines.extend(doc_lines(item.field_doc()));
        if let Shape::Tagged(tagged) = item {
            for variant in &tagged.variants {
                lines.push(assignment_snippet(
                    "<key>",
                    compact(&tagged_variant_json(self, ctx, tagged, variant)?),
                ));
            }
        } else if let Some(tagged) = tagged_array_item(item) {
            for variant in &tagged.variants {
                lines.push(assignment_snippet(
                    "<key>",
                    compact(&Json::Array(vec![tagged_variant_json(
                        self, ctx, tagged, variant,
                    )?])),
                ));
            }
        } else {
            lines.push(assignment_snippet(
                "<key>",
                compact(&example_value(item, "<key>")?),
            ));
        }
        out.insert(COMMENT_PREFIX.into(), comment_value(lines));
        Ok(())
    }

    fn tagged(
        &mut self,
        out: &mut Self::Out,
        ctx: &(),
        name: Option<&str>,
        tagged: &crate::runtime::TaggedShape,
    ) -> Result<(), FormatError> {
        let mut lines = doc_lines(&tagged.doc);
        for variant in &tagged.variants {
            let example = compact(&tagged_variant_json(self, ctx, tagged, variant)?);
            match name {
                Some(name) => lines.push(assignment_snippet(name, example)),
                None => lines.push(example),
            }
        }
        let key = match name {
            Some(name) => comment_key(name),
            None => COMMENT_PREFIX.to_string(),
        };
        if !lines.is_empty() {
            out.insert(key, comment_value(lines));
        }
        Ok(())
    }
}

fn tagged_variant_json(
    renderer: &mut JsonTemplate,
    ctx: &(),
    tagged: &crate::runtime::TaggedShape,
    variant: &crate::runtime::TaggedVariant,
) -> Result<Json, FormatError> {
    let example = tagged_variant_example_schema(tagged, variant);
    let mut obj = JsonMap::new();
    walk_level(renderer, &example, ctx, &mut obj)?;
    Ok(Json::Object(obj))
}

/// The tagged item of `Array { item: Tagged }`, if `shape` is that composition.
fn tagged_array_item(shape: &Shape) -> Option<&crate::runtime::TaggedShape> {
    match shape {
        Shape::Array(array) => match array.item.as_ref() {
            Shape::Tagged(tagged) => Some(tagged),
            _ => None,
        },
        _ => None,
    }
}

/// Example object for an array-of / map-of entry, shown inside a comment:
/// defaults where declared, placeholders elsewhere. `context_key` names
/// the field in conversion errors (non-finite float defaults). Field
/// order follows the schema (not `BTreeMap` sort), so snippets stay
/// byte-identical to declaration order.
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
        obj.insert(nf.name.clone(), example_value(&nf.field, context_key)?);
    }
    Ok(obj)
}

/// One example JSON value for a shape: a declared default when present,
/// otherwise the shared leaf table
/// ([`example_leaf_value`](super::template::example_leaf_value)) or a
/// one-entry nested example (object / array / map). Tagged shapes use
/// the first variant's complete object (callers that need every variant
/// emit them as separate comments).
fn example_value(shape: &Shape, context_key: &str) -> Result<Json, FormatError> {
    match shape {
        Shape::Leaf(leaf) => {
            let value = leaf
                .default
                .clone()
                .unwrap_or_else(|| example_leaf_value(&leaf.ty));
            let mut path = vec![PathSegment::Key(context_key.to_string())];
            value_to_json(&value, &mut path)
        }
        Shape::Object(child) => Ok(Json::Object(example_object(child, context_key)?)),
        Shape::Array(array) => {
            if let Some(default) = &array.default {
                let mut path = vec![PathSegment::Key(context_key.to_string())];
                return value_to_json(default, &mut path);
            }
            Ok(Json::Array(vec![example_value(&array.item, context_key)?]))
        }
        Shape::Map(map) => {
            if let Some(default) = &map.default {
                let mut path = vec![PathSegment::Key(context_key.to_string())];
                return value_to_json(default, &mut path);
            }
            let mut entry = JsonMap::new();
            entry.insert("<key>".to_string(), example_value(&map.item, context_key)?);
            Ok(Json::Object(entry))
        }
        Shape::Tagged(tagged) => {
            let Some(variant) = tagged.variants.first() else {
                return Ok(Json::Object(JsonMap::new()));
            };
            let example = tagged_variant_example_schema(tagged, variant);
            Ok(Json::Object(example_object(&example, context_key)?))
        }
    }
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
    let mut path = vec![PathSegment::Key(key.to_string())];
    Ok(compact(&value_to_json(value, &mut path)?))
}

/// Placeholder rendered in an assignment snippet for a leaf without a
/// default, hinting the expected value shape: the shared table with JSON's
/// quoted spellings for the string and datetime arms.
fn placeholder_json(shape: &crate::runtime::Shape) -> String {
    super::template::placeholder(shape, "\"\"", "\"1970-01-01T00:00:00Z\"")
}

#[cfg(test)]
mod tests {
    use super::super::SetTarget;
    use super::*;

    // --- display spelling ------------------------------------------------

    #[test]
    fn display_entry_json_escapes_the_key() {
        // The ordinary dotted path renders as before…
        assert_eq!(
            JsonAdapter.display_entry("server.host", "localhost"),
            r#""server.host": localhost"#
        );
        // …and a runtime-schema key with JSON-special characters is
        // escaped, not interpolated between bare quotes. Control
        // characters stay one-line (`\n`/`\r`/`\t`/`\uXXXX`), matching
        // the YAML adapter's display contract.
        assert_eq!(JsonAdapter.display_entry(r#"a"b"#, "1"), r#""a\"b": 1"#);
        assert_eq!(JsonAdapter.display_entry(r"a\b", "1"), r#""a\\b": 1"#);
        assert_eq!(JsonAdapter.display_entry("a\nb", "1"), r#""a\nb": 1"#);
        assert_eq!(JsonAdapter.display_entry("a\rb", "1"), r#""a\rb": 1"#);
        assert_eq!(JsonAdapter.display_entry("a\tb", "1"), r#""a\tb": 1"#);
        assert_eq!(
            JsonAdapter.display_entry("a\u{1}b", "1"),
            r#""a\u0001b": 1"#
        );
    }

    // --- capabilities ----------------------------------------------------

    #[test]
    fn json_adapter_declares_its_matrix_rows() {
        // The ADR-0002 matrix has no refusal rows for JSON, so the adapter
        // declares every implemented operation. Spans ride on parse
        // (ADR-0005), not a separate operation.
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
    }

    // --- parse: direct rows ----------------------------------------------

    #[test]
    fn parse_scalars_and_containers() {
        let value = JsonAdapter
            .parse(r#"{"s": "x", "i": 3, "f": 1.5, "b": true, "t": {"n": 1, "arr": [1, 2]}}"#)
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
    fn parse_number_forms() {
        let value = JsonAdapter
            .parse(r#"{"exp": 1e3, "neg": -7, "max": 9223372036854775807}"#)
            .unwrap()
            .value;
        let map = value.as_map().unwrap();
        // An exponent literal is a float, an integer literal an integer.
        assert_eq!(map["exp"], Value::Float(1000.0));
        assert_eq!(map["neg"], Value::Integer(-7));
        assert_eq!(map["max"], Value::Integer(i64::MAX));
    }

    #[test]
    fn parse_string_escapes_and_surrogate_pairs() {
        let value = JsonAdapter
            .parse(r#"{"s": "a\"b\\c\/d\n\u0041\uD83D\uDE00"}"#)
            .unwrap()
            .value;
        assert_eq!(
            value.as_map().unwrap()["s"],
            Value::String("a\"b\\c/d\nA😀".into())
        );
    }

    #[test]
    fn parse_whitespace_only_is_empty_map() {
        assert_eq!(JsonAdapter.parse("").unwrap().value, Value::Map(Map::new()));
        assert_eq!(
            JsonAdapter.parse("  \n\t").unwrap().value,
            Value::Map(Map::new())
        );
    }

    #[test]
    fn parse_non_object_root_maps_shape_faithfully() {
        // Root-shape policy lives in the pipeline (`resolve` rejects
        // non-map roots with one shared error); the adapter maps what the
        // text says.
        assert_eq!(
            JsonAdapter.parse("[1, 2]").unwrap().value,
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
            .unwrap()
            .value;
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
            .unwrap()
            .value;
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
        let source = r#"{"db": {"url": null}}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        match err {
            FormatError::Parse {
                format,
                message,
                span,
            } => {
                assert_eq!(format, "json");
                assert!(message.contains("'db.url'"), "names the key: {message}");
                assert!(
                    message.contains("absence expresses unset"),
                    "advises absence: {message}"
                );
                let span = span.expect("null carries the token span");
                assert_eq!(&source[span.start..span.end], "null");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_null_in_array_names_indexed_path() {
        let source = r#"{"xs": [1, null]}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        assert!(
            err.detail().contains("'xs[1]'"),
            "names the element: {}",
            err.detail()
        );
        let span = err.parse_span().expect("null carries the token span");
        assert_eq!(&source[span.start..span.end], "null");
    }

    #[test]
    fn parse_integer_outside_i64_is_typed_error_naming_key() {
        // i64::MAX + 1 — lexically an integer, outside the value model.
        let source = r#"{"big": 9223372036854775808}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        match err {
            FormatError::Parse { message, span, .. } => {
                assert!(message.contains("'big'"), "names the key: {message}");
                assert!(message.contains("out of range"), "{message}");
                let span = span.expect("out-of-range integer carries its lexeme span");
                assert_eq!(&source[span.start..span.end], "9223372036854775808");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_below_i64_is_typed_error_naming_key() {
        // i64::MIN - 1 — the negative side of the range check. Without the
        // lexical form this would silently coerce to a float.
        let source = r#"{"low": -9223372036854775809}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        match err {
            FormatError::Parse { message, span, .. } => {
                assert!(message.contains("'low'"), "names the key: {message}");
                assert!(message.contains("out of range"), "{message}");
                assert!(message.contains("-9223372036854775809"), "{message}");
                let span = span.expect("out-of-range integer carries its lexeme span");
                assert_eq!(&source[span.start..span.end], "-9223372036854775809");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_above_u64_is_typed_error_naming_key() {
        // u64::MAX + 1 — beyond even serde_json's u64 fallback, still an
        // integer literal, still a typed error.
        let source = r#"{"big": 18446744073709551616}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        assert!(err.detail().contains("'big'"), "{}", err.detail());
        assert!(err.detail().contains("out of range"), "{}", err.detail());
        let span = err.parse_span().expect("lexeme span");
        assert_eq!(&source[span.start..span.end], "18446744073709551616");
    }

    #[test]
    fn parse_float_overflowing_f64_is_typed_error() {
        // A float literal too large for f64 must not become infinity.
        let source = r#"{"huge": 1e999}"#;
        let err = JsonAdapter.parse(source).unwrap_err();
        assert!(err.detail().contains("'huge'"), "{}", err.detail());
        assert!(err.detail().contains("out of range"), "{}", err.detail());
        let span = err.parse_span().expect("lexeme span");
        assert_eq!(&source[span.start..span.end], "1e999");
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
        // The walker tracks byte offsets; with a multi-byte char earlier
        // on the line the span must still land on the offending token,
        // on valid char boundaries (slicing must not panic).
        let source = "{\"héllo\": }";
        let err = JsonAdapter.parse(source).unwrap_err();
        let FormatError::Parse { span, .. } = err else {
            panic!("expected Parse");
        };
        let span = span.expect("syntax errors carry a span");
        assert_eq!(&source[span.start..span.end], "}", "span: {span:?}");
    }

    // --- parse: span index (ADR-0005 / ADR-0006 / ADR-0007) ---------------

    /// Collect every path in `value`, root first — the set a successful
    /// parse's span index must cover.
    fn collect_paths(value: &Value) -> Vec<ConfigPath> {
        fn walk(value: &Value, prefix: ConfigPath, out: &mut Vec<ConfigPath>) {
            out.push(prefix.clone());
            match value {
                Value::Map(map) => {
                    for (key, child) in map {
                        walk(child, prefix.clone().key(key), out);
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(child, prefix.clone().index(i), out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(value, ConfigPath::new(), &mut out);
        out
    }

    fn entry_at<'a>(
        spans: &'a BTreeMap<ConfigPath, SpanEntry>,
        path: &ConfigPath,
    ) -> &'a SpanEntry {
        spans
            .get(path)
            .unwrap_or_else(|| panic!("missing span for {path}"))
    }

    fn slice(source: &str, span: Span) -> &str {
        &source[span.start..span.end]
    }

    #[test]
    fn parse_span_index_covers_every_path_in_nested_objects_and_arrays() {
        // Compact source so every token is unique and the slices lock
        // exact ranges. The unknown-key demo case lives at servers[0].kind.
        let source = r#"{"db":{"url":"pg://x"},"servers":[{"name":"a","kind":"rus"}]}"#;
        let parsed = JsonAdapter.parse(source).unwrap();
        let paths = collect_paths(&parsed.value);
        assert_eq!(
            parsed.spans.len(),
            paths.len(),
            "index paths: {:?}",
            parsed
                .spans
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        for path in &paths {
            assert!(parsed.spans.contains_key(path), "span index missing {path}");
        }

        let root = entry_at(&parsed.spans, &ConfigPath::new());
        assert!(root.key.is_none(), "root has no key token");
        assert_eq!(slice(source, root.value), source);

        let db = entry_at(&parsed.spans, &ConfigPath::new().key("db"));
        assert_eq!(slice(source, db.key.expect("map member")), r#""db""#);
        assert_eq!(slice(source, db.value), r#"{"url":"pg://x"}"#);

        let url = entry_at(&parsed.spans, &ConfigPath::new().key("db").key("url"));
        assert_eq!(slice(source, url.key.expect("map member")), r#""url""#);
        assert_eq!(slice(source, url.value), r#""pg://x""#);

        let servers = entry_at(&parsed.spans, &ConfigPath::new().key("servers"));
        assert_eq!(
            slice(source, servers.key.expect("map member")),
            r#""servers""#
        );
        assert_eq!(
            slice(source, servers.value),
            r#"[{"name":"a","kind":"rus"}]"#
        );

        let server0 = entry_at(&parsed.spans, &ConfigPath::new().key("servers").index(0));
        assert!(
            server0.key.is_none(),
            "array elements have no key token (ADR-0006)"
        );
        assert_eq!(slice(source, server0.value), r#"{"name":"a","kind":"rus"}"#);

        let name = entry_at(
            &parsed.spans,
            &ConfigPath::new().key("servers").index(0).key("name"),
        );
        assert_eq!(slice(source, name.key.expect("map member")), r#""name""#);
        assert_eq!(slice(source, name.value), r#""a""#);
    }

    #[test]
    fn parse_unknown_key_inside_array_of_objects_has_key_span() {
        // Demo lock for WS04: a JSON unknown key inside an array of
        // objects has a correct key span. The caret itself is WS02's
        // unknown-key wiring; this branch locks the index.
        let source = r#"{"servers":[{"name":"a","kind":"rus"}]}"#;
        let parsed = JsonAdapter.parse(source).unwrap();
        let kind = entry_at(
            &parsed.spans,
            &ConfigPath::new().key("servers").index(0).key("kind"),
        );
        let key = kind.key.expect("object member has a key span");
        assert_eq!(slice(source, key), r#""kind""#);
        assert_eq!(slice(source, kind.value), r#""rus""#);
    }

    #[test]
    fn parse_comment_keys_are_absent_from_the_span_index() {
        let source = r#"{
            "//": "top",
            "//host": "docs",
            "host": "localhost",
            "db": {"//url": "docs", "url": "pg://x"},
            "servers": [{"//name": "docs", "name": "a"}]
        }"#;
        let parsed = JsonAdapter.parse(source).unwrap();
        assert!(
            parsed.spans.keys().all(|path| {
                path.segments().iter().all(|seg| match seg {
                    PathSegment::Key(k) => !k.starts_with(COMMENT_PREFIX),
                    PathSegment::Index(_) => true,
                })
            }),
            "comment keys leaked into the index: {:?}",
            parsed
                .spans
                .keys()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        for path in collect_paths(&parsed.value) {
            assert!(
                parsed.spans.contains_key(&path),
                "span index missing {path}"
            );
        }
        assert!(
            parsed.spans.contains_key(&ConfigPath::new().key("host")),
            "real keys stay in the index"
        );
        assert!(!parsed.spans.contains_key(&ConfigPath::new().key("//")));
        assert!(!parsed.spans.contains_key(&ConfigPath::new().key("//host")));
        assert!(
            !parsed
                .spans
                .contains_key(&ConfigPath::new().key("db").key("//url"))
        );
    }

    #[test]
    fn parse_span_index_skips_whitespace_around_tokens() {
        let source = "{ \"host\" : \"x\" }";
        let parsed = JsonAdapter.parse(source).unwrap();
        let host = entry_at(&parsed.spans, &ConfigPath::new().key("host"));
        assert_eq!(slice(source, host.key.expect("map member")), r#""host""#);
        assert_eq!(slice(source, host.value), r#""x""#);
    }

    #[test]
    fn parse_literal_dotted_key_is_one_segment_in_the_span_index() {
        let source = r#"{"a.b": 1}"#;
        let parsed = JsonAdapter.parse(source).unwrap();
        let literal = ConfigPath::new().key("a.b");
        let nested = ConfigPath::new().key("a").key("b");
        let entry = entry_at(&parsed.spans, &literal);
        assert_eq!(slice(source, entry.key.expect("map member")), r#""a.b""#);
        assert_eq!(slice(source, entry.value), "1");
        assert!(!parsed.spans.contains_key(&nested));
    }

    #[test]
    fn parse_duplicate_key_keeps_the_last_value_and_its_spans() {
        let source = r#"{"a":{"x":1},"a":{"y":2}}"#;
        let parsed = JsonAdapter.parse(source).unwrap();
        assert_eq!(
            parsed.value.as_map().unwrap()["a"]
                .as_map()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["y"]
        );
        let a = entry_at(&parsed.spans, &ConfigPath::new().key("a"));
        assert_eq!(slice(source, a.value), r#"{"y":2}"#);
        assert!(
            parsed
                .spans
                .contains_key(&ConfigPath::new().key("a").key("y"))
        );
        assert!(
            !parsed
                .spans
                .contains_key(&ConfigPath::new().key("a").key("x")),
            "replaced subtree must not linger in the index"
        );
    }

    // --- serialize --------------------------------------------------------

    #[test]
    fn serialize_round_trips_parse() {
        let source = r#"{"b": true, "i": 3, "s": "x", "t": {"n": 1}}"#;
        let value = JsonAdapter.parse(source).unwrap().value;
        let text = JsonAdapter.serialize(&value).unwrap();
        assert!(text.ends_with('\n'));
        let reparsed = JsonAdapter.parse(&text).unwrap().value;
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
        let err = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap_err();
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
    "Required.",
    "\"name\": \"\""
  ],
  "//rule": [
    "Any value.",
    "Accepts: any JSON value",
    "Required.",
    "\"rule\": \"\""
  ],
  "db": {
    "//": "Database settings",
    "//url": "\"url\": \"\"",
    "pool_size": 5
  }
}
"#;
        assert_eq!(
            JsonAdapter
                .template(&Shape::Object(schema.clone()))
                .unwrap(),
            golden
        );
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
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
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
    fn template_uses_nested_container_defaults_in_examples() {
        use crate::runtime::{Field, LeafType, Schema as RtSchema};
        let schema = RtSchema::object("App")
            .array_of(
                "plugins",
                RtSchema::object("Plugin").field(
                    "tags",
                    Field::array_of_type(LeafType::String)
                        .default(Value::Array(vec![Value::String("builtin".into())])),
                ),
            )
            .map_of(
                "servers",
                RtSchema::object("Server").field(
                    "labels",
                    Field::map_of(LeafType::String).default({
                        let mut m = Map::new();
                        m.insert("role".into(), Value::String("web".into()));
                        Value::Map(m)
                    }),
                ),
            )
            .build();
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["//plugins"], r#""plugins": [{"tags":["builtin"]}]"#);
        assert_eq!(
            obj["//servers"],
            r#""servers": {"<key>":{"labels":{"role":"web"}}}"#
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
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["//groups"], r#""groups": [{"<key>":{"timeout":30}}]"#);
        assert_eq!(obj["//batches"], r#""batches": {"<key>":[{"timeout":30}]}"#);
    }

    #[test]
    fn template_nested_arrays_keep_every_layer() {
        use crate::runtime::{Field, Schema as RtSchema};
        let item = RtSchema::object("Item").field("timeout", Field::integer().default(30i64));
        let schema = RtSchema::object("App")
            .field(
                "matrix",
                Field::array_of_type(Field::array_of_type(item.clone())),
            )
            .field(
                "cube",
                Field::array_of_type(Field::array_of_type(Field::array_of_type(item))),
            )
            .build();
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let obj = json.as_object().unwrap();
        let parse_snippet = |comment: &Json| {
            let snippet = comment.as_str().expect("single-line comment snippet");
            serde_json::from_str::<Json>(&format!("{{{snippet}}}"))
                .unwrap_or_else(|e| panic!("snippet must parse as JSON object: {e}: {snippet}"))
        };
        assert_eq!(
            parse_snippet(&obj["//matrix"]),
            serde_json::json!({"matrix": [[{"timeout": 30}]]})
        );
        assert_eq!(
            parse_snippet(&obj["//cube"]),
            serde_json::json!({"cube": [[[{"timeout": 30}]]]})
        );
    }

    #[test]
    fn template_root_map_snippet_is_one_entry_not_an_extra_key_level() {
        use crate::runtime::{Field, Schema as RtSchema};
        let item = RtSchema::object("Site")
            .doc("One named site.")
            .field("host", Field::string().default("localhost"))
            .field("port", Field::integer().default(8080i64));
        let shape = Shape::from(Shape::map("sites", item).doc("Named sites by key."));
        let text = JsonAdapter.template(&shape).unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let comment = json
            .as_object()
            .unwrap()
            .get("//")
            .expect("root-map example rides the object comment");
        let lines: Vec<&str> = match comment {
            Json::String(s) => vec![s.as_str()],
            Json::Array(items) => items.iter().filter_map(Json::as_str).collect(),
            other => panic!("comment payload must be a string or array: {other}"),
        };
        assert!(
            lines.iter().any(|l| l.contains("Named sites by key.")),
            "root MapShape docs must appear in the JSON template: {text}"
        );
        assert!(
            lines.iter().any(|l| l.contains("One named site.")),
            "item object docs must still appear: {text}"
        );
        let snippet = lines
            .iter()
            .find(|l| l.contains("\"<key>\""))
            .expect("assignment snippet");
        let parsed: Json = serde_json::from_str(&format!("{{{snippet}}}")).unwrap_or_else(|e| {
            panic!("root-map snippet must parse as a JSON object: {e}: {snippet}")
        });
        let entry = parsed
            .get("<key>")
            .unwrap_or_else(|| panic!("snippet assigns <key>: {parsed}"));
        assert!(
            entry.get("<key>").is_none(),
            "must not wrap the item in a second <key> object: {parsed}"
        );
        assert_eq!(entry["host"], "localhost");
        assert_eq!(entry["port"], 8080);

        let leaf = JsonAdapter
            .template(&Shape::from(Shape::map("values", Field::string())))
            .unwrap();
        let leaf_json: Json = serde_json::from_str(&leaf).unwrap();
        let leaf_comment = &leaf_json["//"];
        let leaf_lines: Vec<&str> = match leaf_comment {
            Json::String(s) => vec![s.as_str()],
            Json::Array(items) => items.iter().filter_map(Json::as_str).collect(),
            other => panic!("leaf comment payload must be a string or array: {other}"),
        };
        let leaf_snippet = leaf_lines
            .iter()
            .find(|l| l.contains("\"<key>\""))
            .expect("leaf assignment snippet");
        let leaf_parsed: Json = serde_json::from_str(&format!("{{{leaf_snippet}}}"))
            .unwrap_or_else(|e| panic!("leaf snippet must parse: {e}: {leaf_snippet}"));
        assert_eq!(leaf_parsed["<key>"], "");
    }

    fn tagged_block() -> crate::runtime::TaggedShape {
        use crate::runtime::{Field, Schema as RtSchema};
        crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "rust",
                RtSchema::object("Rust")
                    .field("mount", Field::string())
                    .build(),
            )
            .variant(
                "payload",
                RtSchema::object("Payload")
                    .field("mount", Field::string())
                    .field("artifact", Field::string())
                    .build(),
            )
            .build()
    }

    fn nested_tagged() -> crate::runtime::TaggedShape {
        use crate::runtime::{Field, Schema as RtSchema};
        let inner = crate::runtime::Shape::tagged("Inner", "kind")
            .variant(
                "alpha",
                RtSchema::object("Alpha")
                    .field("n", Field::integer())
                    .build(),
            )
            .variant(
                "beta",
                RtSchema::object("Beta").field("s", Field::string()).build(),
            )
            .build();
        crate::runtime::Shape::tagged("Outer", "mode")
            .variant(
                "wrap",
                RtSchema::object("Wrap")
                    .field("child", crate::runtime::Shape::from(inner))
                    .build(),
            )
            .build()
    }

    fn comment_lines(comment: &Json) -> Vec<&str> {
        match comment {
            Json::String(s) => vec![s.as_str()],
            Json::Array(items) => items.iter().filter_map(Json::as_str).collect(),
            other => panic!("comment payload must be a string or array: {other}"),
        }
    }

    fn array_of_tagged_schema() -> crate::runtime::Schema {
        use crate::runtime::{Field, Schema as RtSchema};
        RtSchema::object("App")
            .field("blocks", Field::array_of_type(Shape::from(tagged_block())))
            .build()
    }

    fn map_of_array_of_tagged_schema() -> crate::runtime::Schema {
        use crate::runtime::{Field, Schema as RtSchema};
        RtSchema::object("App")
            .field(
                "groups",
                Field::map_of(Shape::array("blocks", Shape::from(tagged_block()))),
            )
            .build()
    }

    fn root_map_of_array_of_tagged() -> crate::runtime::MapShape {
        Shape::map(
            "groups",
            Shape::array("blocks", Shape::from(tagged_block())),
        )
        .build()
    }

    fn snippet_object(line: &str) -> Json {
        serde_json::from_str(&format!("{{{line}}}"))
            .unwrap_or_else(|e| panic!("snippet must parse as a JSON object: {e}: {line}"))
    }

    #[test]
    fn template_tagged_json_snippets_are_copyable_assignments() {
        use crate::runtime::Schema as RtSchema;
        let schema = RtSchema::object("App")
            .field("block", Shape::from(tagged_block()))
            .build();
        let text = JsonAdapter.template(&Shape::Object(schema)).unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//block"]);
        assert_eq!(lines.len(), 2, "{text}");
        for line in &lines {
            assert!(
                line.starts_with("\"block\":"),
                "field snippet must be a JSON assignment, got {line}"
            );
            let parsed: Json = serde_json::from_str(&format!("{{{line}}}"))
                .unwrap_or_else(|e| panic!("snippet must parse as a JSON object: {e}: {line}"));
            assert!(parsed["block"].get("kind").is_some(), "{parsed}");
        }
    }

    #[test]
    fn template_tagged_root_json_snippets_are_complete_objects() {
        let tagged = tagged_block();
        let text = JsonAdapter
            .template(&Shape::Tagged(tagged.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//"]);
        assert_eq!(lines.len(), 2, "{text}");
        for line in &lines {
            let parsed: Json = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("root snippet must be a JSON object: {e}: {line}"));
            assert!(parsed.get("kind").is_some(), "{parsed}");
        }
    }

    #[test]
    fn template_nested_tagged_example_is_a_complete_object() {
        use crate::error::DiscoveryRecord;
        use crate::origin::OriginMap;
        use crate::runtime::DocumentRoot;
        use crate::schema_walk::finalize_root;

        let tagged = nested_tagged();
        let text = JsonAdapter
            .template(&Shape::Tagged(tagged.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//"]);
        let snippet = lines
            .iter()
            .find(|l| l.contains("\"wrap\""))
            .unwrap_or_else(|| panic!("wrap example missing: {text}"));
        let map = match JsonAdapter.parse(snippet).unwrap().value {
            Value::Map(map) => map,
            other => panic!("expected map, got {other:?}"),
        };
        let child = map["child"].as_map().expect("child object");
        assert_eq!(child["kind"], Value::String("alpha".into()));
        finalize_root(
            map,
            &OriginMap::new(),
            DocumentRoot::Tagged(&tagged),
            &DiscoveryRecord::empty(),
        )
        .unwrap();
    }

    #[test]
    fn template_array_of_tagged_snippets_parse_one_variant_at_a_time() {
        use crate::error::DiscoveryRecord;
        use crate::origin::OriginMap;
        use crate::runtime::DocumentRoot;
        use crate::schema_walk::finalize_root;

        let schema = array_of_tagged_schema();
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//blocks"]);
        for kind in ["rust", "payload"] {
            let snippet = lines
                .iter()
                .find(|l| l.contains(&format!("\"{kind}\"")))
                .unwrap_or_else(|| panic!("{kind} example missing: {text}"));
            let parsed = snippet_object(snippet);
            let blocks = parsed["blocks"].as_array().unwrap_or_else(|| {
                panic!("array-of-tagged snippet must be an array, got {parsed}")
            });
            assert_eq!(blocks.len(), 1, "{snippet}");
            assert_eq!(blocks[0]["kind"], kind);
            let map = match JsonAdapter.parse(&format!("{{{snippet}}}")).unwrap().value {
                Value::Map(map) => map,
                other => panic!("expected map, got {other:?}"),
            };
            finalize_root(
                map,
                &OriginMap::new(),
                DocumentRoot::Object(&schema),
                &DiscoveryRecord::empty(),
            )
            .unwrap_or_else(|e| panic!("load {kind} failed: {e}\n{snippet}"));
        }
    }

    #[test]
    fn template_map_of_array_of_tagged_snippets_parse_one_variant_at_a_time() {
        use crate::error::DiscoveryRecord;
        use crate::origin::OriginMap;
        use crate::runtime::DocumentRoot;
        use crate::schema_walk::finalize_root;

        let schema = map_of_array_of_tagged_schema();
        let text = JsonAdapter
            .template(&Shape::Object(schema.clone()))
            .unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//groups"]);
        for kind in ["rust", "payload"] {
            let snippet = lines
                .iter()
                .find(|l| l.contains(&format!("\"{kind}\"")))
                .unwrap_or_else(|| panic!("{kind} example missing: {text}"));
            let parsed = snippet_object(snippet);
            let groups = parsed["groups"].as_object().unwrap_or_else(|| {
                panic!("map-of-array-of-tagged snippet must be an object, got {parsed}")
            });
            let entry = groups
                .get("<key>")
                .or_else(|| groups.values().next())
                .unwrap();
            let items = entry
                .as_array()
                .unwrap_or_else(|| panic!("entry must be an array, got {entry} from {snippet}"));
            assert_eq!(items.len(), 1, "{snippet}");
            assert_eq!(items[0]["kind"], kind);
            let map = match JsonAdapter.parse(&format!("{{{snippet}}}")).unwrap().value {
                Value::Map(map) => map,
                other => panic!("expected map, got {other:?}"),
            };
            finalize_root(
                map,
                &OriginMap::new(),
                DocumentRoot::Object(&schema),
                &DiscoveryRecord::empty(),
            )
            .unwrap_or_else(|e| panic!("load {kind} failed: {e}\n{snippet}"));
        }
    }

    #[test]
    fn template_root_map_of_array_of_tagged_snippets_parse_one_variant_at_a_time() {
        use crate::error::DiscoveryRecord;
        use crate::origin::OriginMap;
        use crate::runtime::DocumentRoot;
        use crate::schema_walk::finalize_root;

        let root = root_map_of_array_of_tagged();
        let text = JsonAdapter.template(&Shape::Map(root.clone())).unwrap();
        let json: Json = serde_json::from_str(&text).unwrap();
        let lines = comment_lines(&json["//"]);
        for kind in ["rust", "payload"] {
            let snippet = lines
                .iter()
                .find(|l| l.contains(&format!("\"{kind}\"")))
                .unwrap_or_else(|| panic!("{kind} example missing: {text}"));
            let assigned = snippet.replace("<key>", "core");
            let parsed = snippet_object(&assigned);
            let entry = parsed.get("core").unwrap_or_else(|| {
                panic!("root-map snippet must assign core, got {parsed} from {snippet}")
            });
            assert!(
                entry.get("core").is_none(),
                "must not wrap the array item in a second key: {parsed}"
            );
            let items = entry.as_array().unwrap_or_else(|| {
                panic!(
                    "root Map<Array<Tagged>> snippet must be an array, got {entry} from {snippet}"
                )
            });
            assert_eq!(items.len(), 1, "{snippet}");
            assert_eq!(items[0]["kind"], kind);
            let map = match JsonAdapter.parse(&format!("{{{assigned}}}")).unwrap().value {
                Value::Map(map) => map,
                other => panic!("expected map, got {other:?}"),
            };
            finalize_root(
                map,
                &OriginMap::new(),
                DocumentRoot::Map(&root),
                &DiscoveryRecord::empty(),
            )
            .unwrap_or_else(|e| panic!("load {kind} failed: {e}\n{snippet}"));
        }
    }

    #[test]
    fn template_parses_clean_through_own_adapter() {
        // gen → parse: every comment key is stripped, every real key is a
        // schema key with its default value.
        use crate::fixtures::test::test_schema;
        let text = JsonAdapter.template(&Shape::Object(test_schema())).unwrap();
        let value = JsonAdapter.parse(&text).unwrap().value;
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
        let tree = JsonAdapter.parse(&edited).unwrap().value;
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
        let reparsed = JsonAdapter.parse(&out).unwrap().value;
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
    fn edit_indexed_path_is_typed_error() {
        let path = ConfigPath::new().key("plugins").index(0).key("host");
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
                assert!(message.contains("[0]"), "{message}");
            }
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
        let out = crate::persist::set_in_document(
            &JsonAdapter,
            &Shape::Object(test_schema()),
            None,
            "port",
            "9090",
            false,
        )
        .unwrap();
        assert!(
            out.contains(r#""//host": "The application host.""#),
            "{out}"
        );
        let tree = JsonAdapter.parse(&out).unwrap().value;
        assert_eq!(tree.as_map().unwrap()["port"], Value::Integer(9090));
    }
}
