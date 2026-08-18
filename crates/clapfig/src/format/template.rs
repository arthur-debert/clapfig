//! The shared schema → template traversal behind every adapter's
//! [`template`](super::FormatAdapter::template).
//!
//! All three formats render the same documented-template shape: per-leaf
//! doc lines plus an `Allowed:` line for enums, an `Accepts:` line for
//! any-value leaves, an `Elements:`/`Values:` element-type hint for
//! array/map leaves, and a `Required.` marker for required-defaultless
//! leaves; a real assignment for defaulted leaves or a commented
//! placeholder otherwise; and commented one-entry examples for
//! array-of/map-of subtrees. That traversal lives here ONCE — the
//! [`walk_level`] driver walks a schema level and dispatches each field to
//! a per-format [`TemplateRenderer`], which owns only the format-specific
//! spelling (TOML's `[section]` headers, YAML's indentation, JSON's
//! comment keys). The leaf annotation lines ([`leaf_annotations`]) and the
//! placeholder table ([`placeholder`]) are shared outright, as are the
//! comment-line helpers the text formats print through.

use std::fmt::Write;

use crate::runtime::{Field, Leaf, LeafType, Schema};
use crate::value::Value;

use super::FormatError;

/// One format's template spelling, driven by [`walk_level`].
///
/// The driver decides *what* to emit for each schema field and in which
/// order; the renderer decides *how* it reads in the format's syntax.
/// Renderer methods for container fields recurse by calling [`walk_level`]
/// again with the child schema and an updated context.
pub(crate) trait TemplateRenderer: Sized {
    /// Per-level rendering context: the dotted section path for TOML, the
    /// indentation depth for YAML, nothing for JSON.
    type Ctx;
    /// The per-level output under construction: text for the comment-line
    /// formats, an object map for JSON.
    type Out;

    /// Whether each level emits its leaves before its container fields.
    /// TOML must (once a `[section]` header is emitted, every following
    /// key belongs to that section until the next header); YAML mirrors it
    /// so cross-format templates read alike; JSON keeps declaration order.
    const LEAVES_FIRST: bool;

    /// Refuse a field name this format cannot spell in a template (JSON's
    /// reserved `//` comment namespace). Every name passes by default.
    fn check_field_name(&self, _name: &str) -> Result<(), FormatError> {
        Ok(())
    }

    /// Emit the level's own doc prose. Only JSON uses this hook (the
    /// `"//"` comment key lives inside the object it documents); the text
    /// formats emit root prose in `template` and child prose in their
    /// container methods, where it must precede the header line.
    fn level_doc(&mut self, _out: &mut Self::Out, _doc: &[String]) {}

    /// Emit one leaf: doc/`Allowed:`/`Accepts:` annotations, then a real
    /// assignment (default present) or a commented placeholder.
    fn leaf(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        leaf: &Leaf,
    ) -> Result<(), FormatError>;

    /// Emit a nested object (TOML `[section]`).
    fn nested(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError>;

    /// Emit an array-of-objects field as a fully commented one-entry
    /// example — clapfig can't know how many entries the user wants.
    fn array_of(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError>;

    /// Emit a map-of-objects field as a fully commented example keyed by a
    /// `<key>` placeholder — entry keys are user-supplied.
    fn map_of(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError>;
}

/// Drive one schema level through `renderer`: the level's doc hook, then
/// every field in the renderer's declared order, each name checked before
/// it is emitted. Container renderer methods call back into this for
/// their child levels.
pub(crate) fn walk_level<R: TemplateRenderer>(
    renderer: &mut R,
    schema: &Schema,
    ctx: &R::Ctx,
    out: &mut R::Out,
) -> Result<(), FormatError> {
    renderer.level_doc(out, &schema.doc);
    if R::LEAVES_FIRST {
        for nf in &schema.fields {
            if let Field::Leaf(leaf) = &nf.field {
                renderer.check_field_name(&nf.name)?;
                renderer.leaf(out, ctx, &nf.name, leaf)?;
            }
        }
    }
    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(leaf) => {
                if !R::LEAVES_FIRST {
                    renderer.check_field_name(&nf.name)?;
                    renderer.leaf(out, ctx, &nf.name, leaf)?;
                }
            }
            Field::Nested(child) => {
                renderer.check_field_name(&nf.name)?;
                renderer.nested(out, ctx, &nf.name, child)?;
            }
            Field::ArrayOf(child) => {
                renderer.check_field_name(&nf.name)?;
                renderer.array_of(out, ctx, &nf.name, child)?;
            }
            Field::MapOf(child) => {
                renderer.check_field_name(&nf.name)?;
                renderer.map_of(out, ctx, &nf.name, child)?;
            }
        }
    }
    Ok(())
}

/// Doc lines ready for a comment payload: trailing whitespace trimmed,
/// blank lines kept as empty strings (paragraph breaks).
pub(crate) fn doc_lines(doc: &[String]) -> Vec<String> {
    doc.iter().map(|line| line.trim_end().to_string()).collect()
}

/// The comment lines annotating one leaf: its doc prose ([`doc_lines`]),
/// an `Allowed:` line listing an enum's (or array-of-enum's per-item)
/// values, an `Accepts:` line for any-value leaves, an `Elements:`/`Values:`
/// element-type hint for array/map leaves (whose placeholders — `[]`/`{}`
/// — carry no type on their own), and a final `Required.` line for leaves
/// the runtime rejects when absent (non-optional with no default — exactly
/// the placeholders the user MUST uncomment). `format_display` names the
/// format in the `Accepts:` line (`"TOML"`); `inline` renders one value
/// in the format's inline spelling (fallible for JSON, whose conversion
/// refuses some values).
pub(crate) fn leaf_annotations(
    leaf: &Leaf,
    format_display: &str,
    inline: &mut dyn FnMut(&Value) -> Result<String, FormatError>,
) -> Result<Vec<String>, FormatError> {
    let mut lines = doc_lines(&leaf.doc);
    // Enum leaves list their value set; array-of-enum leaves
    // (`Vec<UnitEnum>` flattened to `Array(Enum)`) list the same set —
    // the constraint applies per item.
    let enum_values = match &leaf.ty {
        LeafType::Enum { values } => Some(values),
        LeafType::Array(item) => match item.as_ref() {
            LeafType::Enum { values } => Some(values),
            _ => None,
        },
        _ => None,
    };
    if let Some(values) = enum_values {
        let mut listed = Vec::with_capacity(values.len());
        for value in values {
            listed.push(inline(value)?);
        }
        lines.push(format!("Allowed: {}", listed.join(" | ")));
    }
    if matches!(&leaf.ty, LeafType::Value) {
        lines.push(format!("Accepts: any {format_display} value"));
    }
    match &leaf.ty {
        LeafType::Array(elem) => lines.push(format!("Elements: {}", describe_leaf_type(elem))),
        LeafType::Map(elem) => lines.push(format!("Values: {}", describe_leaf_type(elem))),
        _ => {}
    }
    if !leaf.optional && leaf.default.is_none() {
        lines.push("Required.".to_string());
    }
    Ok(lines)
}

/// Human-readable name of a leaf type for the `Elements:`/`Values:`
/// hints, recursing through containers (`array of integer`).
fn describe_leaf_type(ty: &LeafType) -> String {
    match ty {
        LeafType::Array(elem) => format!("array of {}", describe_leaf_type(elem)),
        LeafType::Map(elem) => format!("map of {}", describe_leaf_type(elem)),
        LeafType::Value => "any value".to_string(),
        other => other.name().to_string(),
    }
}

/// Single-word placeholder rendered in a commented-out template line for a
/// leaf without a default, hinting the expected value shape. Two arms vary
/// by format: `string_hint` is the empty-string spelling (`""` for
/// TOML/JSON, `''` for YAML; enums and any-value leaves use it too) and
/// `datetime_hint` the epoch example (quoted in JSON, bare elsewhere).
pub(crate) fn placeholder(
    ty: &LeafType,
    string_hint: &'static str,
    datetime_hint: &'static str,
) -> &'static str {
    match ty {
        LeafType::String | LeafType::Enum { .. } | LeafType::Value => string_hint,
        LeafType::Integer { .. } => "0",
        LeafType::Float => "0.0",
        LeafType::Bool => "false",
        LeafType::DateTime => datetime_hint,
        LeafType::Array(_) => "[]",
        LeafType::Map(_) => "{}",
    }
}

/// Append one doc-comment line at `indent` (`# line`, or `#` alone for
/// blank lines) — the comment spelling both text formats share.
pub(crate) fn push_comment_line(out: &mut String, indent: &str, line: &str) {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        let _ = writeln!(out, "{indent}#");
    } else {
        let _ = writeln!(out, "{indent}# {trimmed}");
    }
}

/// Append `block` with every non-blank line commented out at column zero —
/// uncommenting is deleting the leading `#`, indentation intact.
pub(crate) fn push_commented_block(out: &mut String, block: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(ty: LeafType, doc: &[&str]) -> Leaf {
        Leaf {
            doc: doc.iter().map(ToString::to_string).collect(),
            ty,
            default: None,
            optional: false,
            env: None,
        }
    }

    #[test]
    fn leaf_annotations_orders_doc_then_allowed_then_accepts_then_required() {
        // The fixture leaves are required-defaultless, so the trailing
        // `Required.` marker appears after the type annotations.
        let lines = leaf_annotations(
            &leaf(
                LeafType::Enum {
                    values: vec![Value::from("a"), Value::from("b")],
                },
                &["Doc line.  ", ""],
            ),
            "TOML",
            &mut |v| Ok(format!("<{}>", v.type_str())),
        )
        .unwrap();
        // Doc prose (trailing whitespace trimmed, blanks kept) precedes
        // the Allowed listing, rendered through the per-format closure.
        assert_eq!(
            lines,
            ["Doc line.", "", "Allowed: <string> | <string>", "Required."]
        );

        let lines = leaf_annotations(&leaf(LeafType::Value, &[]), "YAML", &mut |_| {
            unreachable!("no enum values to render")
        })
        .unwrap();
        assert_eq!(lines, ["Accepts: any YAML value", "Required."]);
    }

    #[test]
    fn leaf_annotations_skip_required_for_optional_and_defaulted_leaves() {
        let mut optional = leaf(LeafType::String, &[]);
        optional.optional = true;
        let lines = leaf_annotations(&optional, "TOML", &mut |_| unreachable!()).unwrap();
        assert!(lines.is_empty(), "{lines:?}");

        let mut defaulted = leaf(LeafType::String, &[]);
        defaulted.default = Some(Value::from("x"));
        let lines = leaf_annotations(&defaulted, "TOML", &mut |_| unreachable!()).unwrap();
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn leaf_annotations_hint_container_element_types() {
        let mut arr = leaf(LeafType::Array(Box::new(LeafType::String)), &[]);
        arr.optional = true;
        let lines = leaf_annotations(&arr, "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Elements: string"]);

        let mut nested = leaf(
            LeafType::Array(Box::new(LeafType::Array(Box::new(LeafType::Integer {
                min: None,
                max: None,
            })))),
            &[],
        );
        nested.optional = true;
        let lines = leaf_annotations(&nested, "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Elements: array of integer"]);

        let mut map = leaf(LeafType::Map(Box::new(LeafType::Float)), &[]);
        map.optional = true;
        let lines = leaf_annotations(&map, "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Values: float"]);
    }

    #[test]
    fn placeholder_varies_only_the_string_and_datetime_arms() {
        for ty in [
            LeafType::String,
            LeafType::Enum { values: vec![] },
            LeafType::Value,
        ] {
            assert_eq!(placeholder(&ty, "\"\"", "dt"), "\"\"");
            assert_eq!(placeholder(&ty, "''", "dt"), "''");
        }
        assert_eq!(placeholder(&LeafType::DateTime, "s", "dt-hint"), "dt-hint");
        assert_eq!(
            placeholder(
                &LeafType::Integer {
                    min: None,
                    max: None
                },
                "s",
                "dt"
            ),
            "0"
        );
        assert_eq!(placeholder(&LeafType::Float, "s", "dt"), "0.0");
        assert_eq!(placeholder(&LeafType::Bool, "s", "dt"), "false");
        assert_eq!(
            placeholder(&LeafType::Array(Box::new(LeafType::String)), "s", "dt"),
            "[]"
        );
        assert_eq!(
            placeholder(&LeafType::Map(Box::new(LeafType::String)), "s", "dt"),
            "{}"
        );
    }
}
