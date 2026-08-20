//! The shared schema → template traversal behind every adapter's
//! [`template`](super::FormatAdapter::template).
//!
//! All three formats render the same documented-template shape: per-leaf
//! doc lines plus an `Allowed:` line for enums, an `Accepts:` line for
//! any-value leaves, an `Elements:`/`Values:` element-type hint for
//! array/map leaves, and a `Required.` marker for leaves the runtime
//! rejects when absent (non-optional, defaultless, and neither array-
//! nor map-typed — an absent array/map materializes as `[]`/`{}`); a
//! real assignment for defaulted leaves or a commented placeholder
//! otherwise; and commented one-entry examples for array-of/map-of
//! subtrees. That traversal lives here ONCE — the
//! [`walk_level`] driver walks a schema level and dispatches each field to
//! a per-format [`TemplateRenderer`], which owns only the format-specific
//! spelling (TOML's `[section]` headers, YAML's indentation, JSON's
//! comment keys). The leaf annotation lines ([`leaf_annotations`]) and the
//! placeholder table ([`placeholder`]) are shared outright, as are the
//! comment-line helpers the text formats print through.

use std::fmt::Write;

use crate::runtime::{LeafType, Schema, Shape};
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

    /// Emit one value-shaped field (a leaf, or a homogeneous array/map of
    /// leaves): doc/`Allowed:`/`Accepts:` annotations, then a real
    /// assignment (default present) or a commented placeholder.
    fn leaf(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        field: ValueView<'_>,
    ) -> Result<(), FormatError>;

    /// Emit a nested object (TOML `[section]`).
    fn nested(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        child: &Schema,
    ) -> Result<(), FormatError>;

    /// Emit an array field whose item is not a value shape, as a fully
    /// commented one-entry example — clapfig can't know how many entries
    /// the user wants. `item` may be an object or a nested container;
    /// renderers recurse through Array/Map items, preserving container
    /// syntax at every level.
    fn array_of(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError>;

    /// Emit a map field whose item is not a value shape, as a fully
    /// commented example keyed by a `<key>` placeholder — entry keys are
    /// user-supplied. `item` may be an object or a nested container.
    fn map_of(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: &str,
        item: &Shape,
    ) -> Result<(), FormatError>;

    /// Emit a **document-root** map as a commented example entry, with no
    /// invented parent table. `item` is the entry shape — value-shaped
    /// items (leaf, array/map of leaves) render as a commented assignment;
    /// object and nested-container items keep their format's table /
    /// mapping syntax. `doc` is the root [`MapShape`](crate::runtime::MapShape)'s
    /// own prose (JSON parks it on the `"//"` comment with the example;
    /// TOML/YAML already emit it in `template` before the walk).
    fn root_map(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        item: &Shape,
        doc: &[String],
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
            if nf.field.is_value_field() {
                renderer.check_field_name(&nf.name)?;
                emit_value_field(renderer, out, ctx, &nf.name, &nf.field)?;
            }
        }
    }
    for nf in &schema.fields {
        match &nf.field {
            shape if shape.is_value_field() => {
                if !R::LEAVES_FIRST {
                    renderer.check_field_name(&nf.name)?;
                    emit_value_field(renderer, out, ctx, &nf.name, shape)?;
                }
            }
            Shape::Object(child) => {
                renderer.check_field_name(&nf.name)?;
                renderer.nested(out, ctx, &nf.name, child)?;
            }
            Shape::Array(array) => {
                renderer.check_field_name(&nf.name)?;
                renderer.array_of(out, ctx, &nf.name, &array.item)?;
            }
            Shape::Map(map) => {
                renderer.check_field_name(&nf.name)?;
                renderer.map_of(out, ctx, &nf.name, &map.item)?;
            }
            Shape::Tagged(_) => tagged_template_stub(),
            Shape::Leaf(_) => unreachable!("leaves are value fields"),
        }
    }
    Ok(())
}

/// Drive a document-root [`Shape`] through `renderer`.
///
/// Object roots use [`walk_level`]. Map roots emit a commented example
/// entry with no parent table. Tagged is SHP01-WS05. Leaf and Array are
/// illegal document roots.
pub(crate) fn walk_root<R: TemplateRenderer>(
    renderer: &mut R,
    shape: &Shape,
    ctx: &R::Ctx,
    out: &mut R::Out,
) -> Result<(), FormatError> {
    match shape {
        Shape::Object(schema) => walk_level(renderer, schema, ctx, out),
        Shape::Map(map) => renderer.root_map(out, ctx, &map.item, &map.doc),
        Shape::Tagged(_) => {
            tagged_template_stub();
        }
        Shape::Leaf(_) | Shape::Array(_) => panic!(
            "clapfig: a Leaf or Array is not a legal document root (legal roots: Object, Map, Tagged)"
        ),
    }
}

/// A value-shaped field as the template leaf renderer sees it: a leaf,
/// or a homogeneous array/map of leaves.
#[derive(Clone, Copy)]
pub(crate) struct ValueView<'a> {
    pub doc: &'a [String],
    pub default: Option<&'a Value>,
    pub optional: bool,
    pub shape: &'a Shape,
}

impl<'a> ValueView<'a> {
    pub(crate) fn from_shape(shape: &'a Shape) -> Self {
        match shape {
            Shape::Leaf(leaf) => Self {
                doc: &leaf.doc,
                default: leaf.default.as_ref(),
                optional: leaf.optional,
                shape,
            },
            Shape::Array(array) => Self {
                doc: &array.doc,
                default: array.default.as_ref(),
                optional: array.optional,
                shape,
            },
            Shape::Map(map) => Self {
                doc: &map.doc,
                default: map.default.as_ref(),
                optional: map.optional,
                shape,
            },
            Shape::Object(_) | Shape::Tagged(_) => unreachable!("not a value field"),
        }
    }
}

fn emit_value_field<R: TemplateRenderer>(
    renderer: &mut R,
    out: &mut R::Out,
    ctx: &R::Ctx,
    name: &str,
    shape: &Shape,
) -> Result<(), FormatError> {
    renderer.leaf(out, ctx, name, ValueView::from_shape(shape))
}

/// Tagged template emission is SHP01-WS05. Walkers in this slice fail
/// loudly rather than emit a lying example.
pub(crate) fn tagged_template_stub() -> ! {
    panic!(
        "clapfig: tagged templates are SHP01-WS05; object-root schemas in this slice have no tagged fields"
    );
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
/// the runtime rejects when absent (non-optional, defaultless, neither
/// array- nor map-typed — the placeholders the user MUST uncomment).
/// Absent array/map leaves materialize as `[]`/`{}`, so they do not get
/// the line; JSON Schema `required` uses the same rule. `format_display`
/// names the format in the `Accepts:` line (`"TOML"`); `inline` renders
/// one value in the format's inline spelling (fallible for JSON, whose
/// conversion refuses some values).
pub(crate) fn leaf_annotations(
    field: ValueView<'_>,
    format_display: &str,
    inline: &mut dyn FnMut(&Value) -> Result<String, FormatError>,
) -> Result<Vec<String>, FormatError> {
    let mut lines = doc_lines(field.doc);
    // Enum leaves list their value set; array-of-enum fields
    // (`Vec<UnitEnum>`) list the same set — the constraint applies per
    // item.
    if let Some(values) = enum_values_of(field.shape) {
        let mut listed = Vec::with_capacity(values.len());
        for value in values {
            listed.push(inline(value)?);
        }
        lines.push(format!("Allowed: {}", listed.join(" | ")));
    }
    if matches!(field.shape, Shape::Leaf(leaf) if matches!(leaf.ty, LeafType::Value)) {
        lines.push(format!("Accepts: any {format_display} value"));
    }
    match field.shape {
        Shape::Array(array) => {
            lines.push(format!("Elements: {}", describe_shape_item(&array.item)))
        }
        Shape::Map(map) => lines.push(format!("Values: {}", describe_shape_item(&map.item))),
        _ => {}
    }
    // Same absence rule as JSON Schema `required` and `fill_defaults_into`:
    // an absent non-optional array/map materializes as `[]`/`{}`, so the
    // runtime does not reject it and the template must not mark it
    // required.
    if !field.optional
        && field.default.is_none()
        && !matches!(field.shape, Shape::Array(_) | Shape::Map(_))
    {
        lines.push("Required.".to_string());
    }
    Ok(lines)
}

fn enum_values_of(shape: &Shape) -> Option<&[Value]> {
    match shape {
        Shape::Leaf(leaf) => match &leaf.ty {
            LeafType::Enum { values } => Some(values),
            _ => None,
        },
        Shape::Array(array) => enum_values_of(&array.item),
        _ => None,
    }
}

/// Human-readable name of an item shape for the `Elements:`/`Values:`
/// hints, recursing through containers (`array of integer`).
fn describe_shape_item(shape: &Shape) -> String {
    match shape {
        Shape::Array(array) => format!("array of {}", describe_shape_item(&array.item)),
        Shape::Map(map) => format!("map of {}", describe_shape_item(&map.item)),
        Shape::Leaf(leaf) => match &leaf.ty {
            LeafType::Value => "any value".to_string(),
            other => other.name().to_string(),
        },
        Shape::Object(_) => "object".to_string(),
        Shape::Tagged(_) => "tagged".to_string(),
    }
}

/// Single-word placeholder rendered in a commented-out template line for a
/// leaf without a default, hinting the expected value shape. Two arms vary
/// by format: `string_hint` is the empty-string spelling (`""` for
/// TOML/JSON, `''` for YAML; enums and any-value leaves use it too) and
/// `datetime_hint` the epoch example (quoted in JSON, bare elsewhere).
pub(crate) fn placeholder(
    shape: &Shape,
    string_hint: &'static str,
    datetime_hint: &'static str,
) -> &'static str {
    match shape {
        Shape::Array(_) => "[]",
        Shape::Map(_) => "{}",
        Shape::Leaf(leaf) => match &leaf.ty {
            LeafType::String | LeafType::Enum { .. } | LeafType::Value => string_hint,
            LeafType::Integer { .. } => "0",
            LeafType::Float => "0.0",
            LeafType::Bool => "false",
            LeafType::DateTime => datetime_hint,
        },
        Shape::Object(_) | Shape::Tagged(_) => unreachable!("not a value field"),
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
    use crate::runtime::Field;

    fn view(shape: &Shape) -> ValueView<'_> {
        ValueView::from_shape(shape)
    }

    #[test]
    fn leaf_annotations_orders_doc_then_allowed_then_accepts_then_required() {
        // The fixture leaves are required-defaultless, so the trailing
        // `Required.` marker appears after the type annotations.
        let shape = Shape::from(Field::enum_of(["a", "b"]).doc("Doc line.  ").doc(""));
        let lines = leaf_annotations(view(&shape), "TOML", &mut |v| {
            Ok(format!("<{}>", v.type_str()))
        })
        .unwrap();
        // Doc prose (trailing whitespace trimmed, blanks kept) precedes
        // the Allowed listing, rendered through the per-format closure.
        assert_eq!(
            lines,
            ["Doc line.", "", "Allowed: <string> | <string>", "Required."]
        );

        let shape = Shape::from(Field::value());
        let lines = leaf_annotations(view(&shape), "YAML", &mut |_| {
            unreachable!("no enum values to render")
        })
        .unwrap();
        assert_eq!(lines, ["Accepts: any YAML value", "Required."]);
    }

    #[test]
    fn leaf_annotations_skip_required_for_optional_and_defaulted_leaves() {
        let optional = Shape::from(Field::string().optional());
        let lines = leaf_annotations(view(&optional), "TOML", &mut |_| unreachable!()).unwrap();
        assert!(lines.is_empty(), "{lines:?}");

        let defaulted = Shape::from(Field::string().default("x"));
        let lines = leaf_annotations(view(&defaulted), "TOML", &mut |_| unreachable!()).unwrap();
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn leaf_annotations_hint_container_element_types() {
        // Non-optional, defaultless array/map leaves still skip `Required.`:
        // absence materializes as `[]`/`{}`, matching JSON Schema `required`.
        let arr = Shape::from(Field::array_of_type(LeafType::String));
        let lines = leaf_annotations(view(&arr), "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Elements: string"]);

        let nested = Shape::from(Field::array_of_type(Field::array_of_type(Field::integer())));
        let lines = leaf_annotations(view(&nested), "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Elements: array of integer"]);

        let map = Shape::from(Field::map_of(LeafType::Float));
        let lines = leaf_annotations(view(&map), "TOML", &mut |_| unreachable!()).unwrap();
        assert_eq!(lines, ["Values: float"]);
    }

    #[test]
    fn placeholder_varies_only_the_string_and_datetime_arms() {
        for shape in [
            Shape::from(Field::string()),
            Shape::from(Field::enum_of(Vec::<&str>::new())),
            Shape::from(Field::value()),
        ] {
            assert_eq!(placeholder(&shape, "\"\"", "dt"), "\"\"");
            assert_eq!(placeholder(&shape, "''", "dt"), "''");
        }
        assert_eq!(
            placeholder(&Shape::from(Field::datetime()), "s", "dt-hint"),
            "dt-hint"
        );
        assert_eq!(placeholder(&Shape::from(Field::integer()), "s", "dt"), "0");
        assert_eq!(placeholder(&Shape::from(Field::float()), "s", "dt"), "0.0");
        assert_eq!(
            placeholder(&Shape::from(Field::boolean()), "s", "dt"),
            "false"
        );
        assert_eq!(
            placeholder(
                &Shape::from(Field::array_of_type(LeafType::String)),
                "s",
                "dt"
            ),
            "[]"
        );
        assert_eq!(
            placeholder(&Shape::from(Field::map_of(LeafType::String)), "s", "dt"),
            "{}"
        );
    }
}
