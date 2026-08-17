//! YAML format adapter — `serde_norway` parsing, `yamlpath`/`yamlpatch`
//! editing (ADR-0003).
//!
//! This file is the ONLY place in the crate (outside `Cargo.toml`) that
//! touches the YAML crates: `serde_norway` (parse/serialize; the
//! `yaml_serde` sibling exists solely because `yamlpatch`'s patch values
//! are its type) and `yamlpath` + `yamlpatch` (targeted span-level edits).
//!
//! Baseline mapping (ADR-0002's table, YAML rows):
//!
//! - **Strict scalars** — only `true`/`false` spellings are booleans; `no`,
//!   `yes`, `on`, `off` parse as strings (no Norway problem; locked by
//!   tests).
//! - **Aliases** resolve at parse, invisible to the model; **custom tags**
//!   and **merge keys** (`<<`) are typed errors naming the offending key.
//! - **`null`/`~`** is a typed error advising absence; an empty or
//!   comments-only document is the empty map (absence, not null).
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

use std::fmt::Write;

use crate::runtime::{Field, LeafType, Schema};
use crate::value::{Map, Value};

use super::{
    ConfigPath, FileEdit, FormatAdapter, FormatError, Operation, PathSegment, Span, SpanIndex,
    UnsupportedByFormat,
};

/// The YAML format behind the adapter contract.
///
/// Declares every operation except [`Operation::SpanIndex`] (undeclared and
/// refused typed until the provenance epic builds the index). The declared
/// edit operations still refuse specific shapes at runtime — sequence-item
/// edits and flow-style shapes the patch stack cannot rewrite honestly —
/// per ADR-0002's known-refusals row; see the [module docs](self).
pub struct YamlAdapter;

impl FormatAdapter for YamlAdapter {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
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
        // An empty or comments-only file is an empty config: absence, not
        // null. (serde_norway parses both to a root Null; only a document
        // with actual non-comment content gets the null error below.)
        if is_blank_or_comments(text) {
            return Ok(Value::Map(Map::new()));
        }
        let raw: serde_norway::Value =
            serde_norway::from_str(text).map_err(|e| parse_error(&e, text))?;
        norway_to_value(raw, &mut Vec::new())
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
        emit_schema(&mut out, schema, 0);
        Ok(out)
    }

    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        let operation = edit.operation();
        match edit {
            FileEdit::Set { path, value, .. } => {
                let keys = key_segments(path, operation)?;
                set_in_source(source, &keys, value, operation)
            }
            FileEdit::Unset { path } => {
                let keys = key_segments(path, Operation::EditUnset)?;
                unset_in_source(source, &keys)
            }
        }
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        // Provenance epic: build the path → span index from parser spans.
        Err(UnsupportedByFormat {
            format: self.name(),
            operation: Operation::SpanIndex,
        }
        .into())
    }
}

/// `true` when every line is blank or a `#` comment — the "no content"
/// document that parses to the empty map instead of the null error.
fn is_blank_or_comments(text: &str) -> bool {
    text.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    })
}

/// Map a `serde_norway` parse failure into the shared error, carrying the
/// reported location as a (single-byte) span when present.
fn parse_error(e: &serde_norway::Error, text: &str) -> FormatError {
    FormatError::Parse {
        format: "yaml",
        message: e.to_string(),
        span: e.location().map(|l| {
            let start = l.index().min(text.len());
            Span {
                start,
                end: (start + 1).min(text.len()).max(start),
            }
        }),
    }
}

/// Render the path built up during conversion for an error message; the
/// document root has no path to name.
fn path_label(path: &[PathSegment]) -> String {
    if path.is_empty() {
        "the document root".to_string()
    } else {
        format!("'{}'", ConfigPath::from(path.to_vec()))
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
            path_label(path)
        ))),
        serde_norway::Value::Bool(b) => Ok(Value::Boolean(b)),
        serde_norway::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if n.is_u64() {
                Err(mapping_error(format!(
                    "integer {n} at {} is out of range: integers are 64-bit signed",
                    path_label(path)
                )))
            } else {
                Ok(Value::Float(
                    n.as_f64().expect("numbers are i64, u64, or f64"),
                ))
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
                        path_label(path)
                    )));
                };
                if key == "<<" {
                    return Err(mapping_error(format!(
                        "YAML merge key '<<' at {} is outside the configuration baseline: spell the keys out",
                        path_label(path)
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
            path_label(path)
        ))),
    }
}

/// Best-effort inline rendering of a raw YAML value for error messages.
fn inline_norway(value: &serde_norway::Value) -> String {
    serde_norway::to_string(value)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_else(|_| format!("{value:?}"))
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
        Value::Datetime(d) => serde_norway::Value::String(d.to_string()),
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

/// Extract the key segments of a [`ConfigPath`], refusing array-index
/// segments: sequence-item edits (replace or append) are ADR-0002's known
/// YAML refusals — `yamlpatch` cannot patch them honestly.
fn key_segments(path: &ConfigPath, operation: Operation) -> Result<Vec<&str>, FormatError> {
    path.segments()
        .iter()
        .map(|seg| match seg {
            PathSegment::Key(k) => Ok(k.as_str()),
            PathSegment::Index(_) => Err(FormatError::Unsupported(UnsupportedByFormat {
                format: "yaml",
                operation,
            })),
        })
        .collect()
}

/// Convert an owned [`Value`] into a `yaml_serde::Value` — the type
/// `yamlpatch` patch operations carry. Same rules as [`value_to_norway`].
fn value_to_patch(value: &Value) -> yaml_serde::Value {
    match value {
        Value::String(s) => yaml_serde::Value::String(s.clone()),
        Value::Integer(i) => yaml_serde::Value::Number((*i).into()),
        Value::Float(f) => yaml_serde::Value::Number((*f).into()),
        Value::Boolean(b) => yaml_serde::Value::Bool(*b),
        Value::Datetime(d) => yaml_serde::Value::String(d.to_string()),
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
        Value::Datetime(d) => Value::String(d.to_string()),
        Value::Array(items) => Value::Array(items.iter().map(as_parsed).collect()),
        Value::Map(map) => Value::Map(map.iter().map(|(k, v)| (k.clone(), as_parsed(v))).collect()),
        other => other.clone(),
    }
}

/// Tree equality with `NaN == NaN`, so verification of an edit that writes
/// a non-finite float does not refuse over IEEE 754 inequality.
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

/// Insert `value` at `keys` in `map`, creating intermediate maps. Errors on
/// a path conflict — an existing non-map value where the path needs a map.
fn set_in_tree(map: &mut Map, keys: &[&str], value: Value) -> Result<(), FormatError> {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let display_path = keys.join(".");
    let mut current = map;
    for segment in parents {
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| Value::Map(Map::new()));
        match entry {
            Value::Map(next) => current = next,
            _ => {
                return Err(FormatError::Edit {
                    format: "yaml",
                    message: format!(
                        "path conflict: existing file has a non-map value at '{segment}' (setting '{display_path}')"
                    ),
                });
            }
        }
    }
    current.insert(leaf.to_string(), value);
    Ok(())
}

/// Remove `keys` from `map`; `false` when the path was already absent.
fn remove_from_tree(map: &mut Map, keys: &[&str]) -> bool {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let mut current = map;
    for segment in parents {
        match current.get_mut(*segment) {
            Some(Value::Map(next)) => current = next,
            _ => return false,
        }
    }
    current.remove(*leaf).is_some()
}

/// Parse the file under edit into its value tree. Empty and comments-only
/// sources are the empty map (same rule as [`YamlAdapter::parse`]).
fn parse_edit_source(source: &str) -> Result<Map, FormatError> {
    match YamlAdapter.parse(source)? {
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

    // Nothing in the file yet: append the serialized subtree, preserving
    // any existing comment lines (the template-seeded create-file case).
    if original.is_empty() {
        let mut fresh = Map::new();
        set_in_tree(&mut fresh, keys, value.clone())?;
        let rendered = YamlAdapter.serialize(&Value::Map(fresh))?;
        let mut out = source.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&rendered);
        return Ok(out);
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
/// returned unchanged, mirroring the TOML adapter.
fn unset_in_source(source: &str, keys: &[&str]) -> Result<String, FormatError> {
    let original = parse_edit_source(source)?;
    let mut expected = original.clone();
    if !remove_from_tree(&mut expected, keys) {
        return Ok(source.to_string());
    }
    let patches = [yamlpatch::Patch {
        route: route_for(keys),
        operation: yamlpatch::Op::Remove,
    }];
    apply_and_verify(
        source,
        &patches,
        &expected,
        &keys.join("."),
        Operation::EditUnset,
    )
}

// --- template emission (native YAML comments) -----------------------------

/// Append one doc-comment line at `indent` (`#` alone for blank lines).
fn push_comment_line(out: &mut String, indent: &str, line: &str) {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        let _ = writeln!(out, "{indent}#");
    } else {
        let _ = writeln!(out, "{indent}# {trimmed}");
    }
}

/// Append `block` with every non-blank line commented out at column zero —
/// uncommenting is deleting the leading `#`, indentation intact.
fn push_commented_block(out: &mut String, block: &str) {
    for line in block.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push('#');
            out.push_str(line);
            out.push('\n');
        }
    }
}

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

fn emit_schema(out: &mut String, schema: &Schema, depth: usize) {
    let indent = "  ".repeat(depth);

    // Local leaves first, then subsections — the same layout as the TOML
    // template, so cross-format templates read alike. (YAML's indentation
    // scoping does not require it.)
    for nf in &schema.fields {
        let Field::Leaf(leaf) = &nf.field else {
            continue;
        };
        for line in &leaf.doc {
            push_comment_line(out, &indent, line);
        }
        if let LeafType::Enum { values } = &leaf.ty {
            let listed = values
                .iter()
                .map(format_inline_yaml)
                .collect::<Vec<_>>()
                .join(" | ");
            let _ = writeln!(out, "{indent}# Allowed: {listed}");
        }
        if matches!(&leaf.ty, LeafType::Value) {
            let _ = writeln!(out, "{indent}# Accepts: any YAML value");
        }
        match &leaf.default {
            Some(value) => {
                let _ = writeln!(out, "{indent}{}: {}", nf.name, format_inline_yaml(value));
            }
            None => {
                let hint = template_placeholder(&leaf.ty);
                let _ = writeln!(out, "{indent}#{}: {hint}", nf.name);
            }
        }
        out.push('\n');
    }

    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(_) => {} // already emitted above
            Field::Nested(child) => {
                for line in &child.doc {
                    push_comment_line(out, &indent, line);
                }
                if has_active_content(child) {
                    let _ = writeln!(out, "{indent}{}:", nf.name);
                    emit_schema(out, child, depth + 1);
                } else {
                    // All-commented section: comment the key too, or the
                    // generated document would parse it as null.
                    let mut buf = String::new();
                    let _ = writeln!(buf, "{indent}{}:", nf.name);
                    emit_schema(&mut buf, child, depth + 1);
                    push_commented_block(out, &buf);
                }
            }
            Field::ArrayOf(child) => {
                for line in &child.doc {
                    push_comment_line(out, &indent, line);
                }
                // Array-of-objects renders one fully commented example
                // item — clapfig can't know how many entries the user
                // wants.
                let mut buf = String::new();
                let _ = writeln!(buf, "{indent}{}:", nf.name);
                let mut item = String::new();
                emit_schema(&mut item, child, depth + 2);
                buf.push_str(&with_sequence_dash(&item, depth + 1));
                push_commented_block(out, &buf);
            }
            Field::MapOf(child) => {
                for line in &child.doc {
                    push_comment_line(out, &indent, line);
                }
                // Map-of-objects: entry keys are user-supplied, so the
                // example uses a placeholder entry name, fully commented.
                let mut buf = String::new();
                let _ = writeln!(buf, "{indent}{}:", nf.name);
                let _ = writeln!(buf, "{indent}  <key>:");
                emit_schema(&mut buf, child, depth + 2);
                push_commented_block(out, &buf);
            }
        }
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
/// double-quoted escape, since the library would emit a block scalar —
/// unusable inline.
fn inline_scalar(s: &str) -> String {
    if s.chars().any(|c| c.is_control()) {
        return format!("{s:?}");
    }
    inline_norway(&serde_norway::Value::String(s.to_string()))
}

/// Single-word placeholder rendered in a commented-out template line for a
/// leaf without a default, hinting the expected value shape.
fn template_placeholder(ty: &LeafType) -> &'static str {
    match ty {
        LeafType::String => "''",
        LeafType::Integer => "0",
        LeafType::Float => "0.0",
        LeafType::Bool => "false",
        LeafType::DateTime => "1970-01-01T00:00:00Z",
        LeafType::Array(_) => "[]",
        LeafType::Map(_) => "{}",
        LeafType::Enum { .. } => "''",
        LeafType::Value => "''",
    }
}

#[cfg(test)]
mod tests {
    use super::super::SetTarget;
    use super::*;

    fn parse_map(text: &str) -> Map {
        match YamlAdapter.parse(text).expect("fixture must parse") {
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
        assert_eq!(YamlAdapter.parse("").unwrap(), Value::Map(Map::new()));
        assert_eq!(
            YamlAdapter.parse("# just\n\n# comments\n").unwrap(),
            Value::Map(Map::new())
        );
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

    // --- serialization ---

    #[test]
    fn serialize_round_trips_parse() {
        let source = "b: true\ni: 3\ns: x\nt:\n  n: 1\n";
        let value = YamlAdapter.parse(source).unwrap();
        let text = YamlAdapter.serialize(&value).unwrap();
        let reparsed = YamlAdapter.parse(&text).unwrap();
        assert_eq!(value, reparsed);
    }

    #[test]
    fn serialize_quotes_strings_the_parser_would_mistype() {
        let mut map = Map::new();
        map.insert("port_str".into(), Value::String("8080".into()));
        map.insert("country".into(), Value::String("no".into()));
        let text = YamlAdapter.serialize(&Value::Map(map.clone())).unwrap();
        assert_eq!(YamlAdapter.parse(&text).unwrap(), Value::Map(map));
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
#name: ''

# Any value.
# Accepts: any YAML value
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
    fn edit_refuses_sequence_item_replace() {
        // Sequence-item replace is a known refusal: index segments are
        // refused typed, under the request's own matrix row.
        let path = ConfigPath::new().key("servers").index(0).key("host");
        let value = Value::from("x");
        let err = YamlAdapter
            .edit(
                "servers:\n  - host: a\n",
                set_edit(&path, &value, SetTarget::ExistingValue),
            )
            .unwrap_err();
        match err {
            FormatError::Unsupported(u) => {
                assert_eq!(u.format, "yaml");
                assert_eq!(u.operation, Operation::EditSet);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn edit_refuses_list_append_via_index() {
        // Appending (an index one past the end) is the same refusal family
        // as flow-style list append: no honest span patch exists.
        let path = ConfigPath::new().key("servers").index(2);
        let value = Value::from("c");
        let err = YamlAdapter
            .edit(
                "servers: [a, b]\n",
                set_edit(&path, &value, SetTarget::MissingKey),
            )
            .unwrap_err();
        match err {
            FormatError::Unsupported(u) => assert_eq!(u.operation, Operation::EditCreateKey),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

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
