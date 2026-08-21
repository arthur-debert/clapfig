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
//! comment keys). The leaf annotation lines ([`leaf_annotations`]) are
//! shared outright, as are the comment-line helpers the text formats
//! print through. The single example-value table is
//! [`example_leaf_value`]: [`placeholder`] renders it, and
//! [`with_example_defaults`] / [`example_shape_value`] consume it.

use std::fmt::Write;

use crate::runtime::{Field, LeafType, Schema, Shape, TaggedShape, TaggedVariant};
use crate::value::{Map, Value};

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

    /// Emit one commented example per tagged variant.
    ///
    /// `name` is the field name in field position, or `None` at a tagged
    /// document root. Each example is a complete object for that
    /// discriminator (tag plus that variant's fields). No uncommented
    /// mixed-variant object is emitted.
    fn tagged(
        &mut self,
        out: &mut Self::Out,
        ctx: &Self::Ctx,
        name: Option<&str>,
        tagged: &TaggedShape,
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
            Shape::Tagged(tagged) => {
                renderer.check_field_name(&nf.name)?;
                renderer.tagged(out, ctx, Some(&nf.name), tagged)?;
            }
            Shape::Leaf(_) => unreachable!("leaves are value fields"),
        }
    }
    Ok(())
}

/// Drive a document-root [`Shape`] through `renderer`.
///
/// Object roots use [`walk_level`]. Map roots emit a commented example
/// entry with no parent table. Tagged roots emit one commented example
/// per variant. Leaf and Array are illegal document roots.
pub(crate) fn walk_root<R: TemplateRenderer>(
    renderer: &mut R,
    shape: &Shape,
    ctx: &R::Ctx,
    out: &mut R::Out,
) -> Result<(), FormatError> {
    match shape {
        Shape::Object(schema) => walk_level(renderer, schema, ctx, out),
        Shape::Map(map) => renderer.root_map(out, ctx, &map.item, &map.doc),
        Shape::Tagged(tagged) => renderer.tagged(out, ctx, None, tagged),
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

/// Build a walkable object schema for one tagged variant: the tag field
/// (defaulted to the discriminator) plus the variant's fields with
/// placeholder defaults so a single `walk_level` pass emits uncommented
/// assignments that the caller then comments as a block.
///
/// Nested tagged fields are materialized as the first child variant's
/// complete object so the enclosing example is valid when uncommented.
pub(crate) fn tagged_variant_example_schema(
    tagged: &TaggedShape,
    variant: &TaggedVariant,
) -> Schema {
    let mut builder = Schema::object(variant.schema.name.clone());
    for line in &variant.schema.doc {
        builder = builder.doc(line.clone());
    }
    builder = builder.field(
        tagged.tag.clone(),
        Field::string().default(variant.discriminator.clone()),
    );
    for nf in &variant.schema.fields {
        builder = builder.field(nf.name.clone(), with_example_defaults(&nf.field));
    }
    builder.build()
}

fn with_example_defaults(shape: &Shape) -> Shape {
    match shape {
        Shape::Leaf(leaf) => {
            let mut leaf = leaf.clone();
            if leaf.default.is_none() {
                leaf.default = Some(example_leaf_value(&leaf.ty));
            }
            Shape::Leaf(leaf)
        }
        Shape::Object(schema) => {
            let mut builder = Schema::object(schema.name.clone());
            for line in &schema.doc {
                builder = builder.doc(line.clone());
            }
            if let Some(strict) = schema.strict {
                builder = builder.strict(strict);
            }
            for nf in &schema.fields {
                builder = builder.field(nf.name.clone(), with_example_defaults(&nf.field));
            }
            Shape::Object(builder.build())
        }
        Shape::Array(array) => {
            let mut array = array.clone();
            array.item = Box::new(with_example_defaults(&array.item));
            Shape::Array(array)
        }
        Shape::Map(map) => {
            let mut map = map.clone();
            map.item = Box::new(with_example_defaults(&map.item));
            Shape::Map(map)
        }
        Shape::Tagged(tagged) => tagged
            .variants
            .first()
            .map(|variant| Shape::Object(tagged_variant_example_schema(tagged, variant)))
            .unwrap_or_else(|| Shape::Tagged(tagged.clone())),
    }
}

/// One-entry example [`Value`] for a shape: declared defaults win,
/// otherwise a constraint-satisfying leaf ([`example_leaf_value`]) or a
/// one-entry nested example (container). Used when a template must keep
/// nested array layers in an inline assignment.
pub(crate) fn example_shape_value(shape: &Shape) -> Value {
    match shape {
        Shape::Leaf(leaf) => leaf
            .default
            .clone()
            .unwrap_or_else(|| example_leaf_value(&leaf.ty)),
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
        Shape::Tagged(tagged) => tagged
            .variants
            .first()
            .map(|variant| {
                let example = tagged_variant_example_schema(tagged, variant);
                let mut map = Map::new();
                for nf in &example.fields {
                    map.insert(nf.name.clone(), example_shape_value(&nf.field));
                }
                Value::Map(map)
            })
            .unwrap_or_else(|| Value::Map(Map::new())),
    }
}

/// Constraint-satisfying example value for a leaf: the first enum
/// member, an integer inside declared bounds, otherwise a typed
/// zero/empty. This is the single example-value table;
/// [`placeholder`] renders it and [`example_shape_value`] /
/// [`with_example_defaults`] consume it.
pub(crate) fn example_leaf_value(ty: &LeafType) -> Value {
    match ty {
        LeafType::String | LeafType::Value => Value::String(String::new()),
        LeafType::Enum { values } => values
            .first()
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
        LeafType::Integer { min, max } => Value::Integer(example_integer(*min, *max)),
        LeafType::Float => Value::Float(0.0),
        LeafType::Bool => Value::Boolean(false),
        LeafType::DateTime => Value::Datetime(
            "1970-01-01T00:00:00Z"
                .parse()
                .expect("epoch datetime placeholder is valid"),
        ),
    }
}

/// Placeholder integer that satisfies declared bounds: zero when it is
/// in range, otherwise the lower bound of a positive-only range or the
/// upper bound of a negative-only range.
fn example_integer(min: Option<i64>, max: Option<i64>) -> i64 {
    if min.is_none_or(|lo| 0 >= lo) && max.is_none_or(|hi| 0 <= hi) {
        0
    } else if let Some(lo) = min.filter(|lo| *lo > 0) {
        lo
    } else {
        max.or(min).unwrap_or(0)
    }
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

/// Commented-out template spelling of a defaultless value-shaped field.
///
/// Arrays and maps stay `[]` / `{}` (empty containers, not one-entry
/// examples). Leaves render [`example_leaf_value`] through `inline` —
/// the same format-specific encoder that `Allowed:` listings and
/// default assignments already use — so an enum member is a legal,
/// escaped spelling of that value (a float stays `1.5`, a string with
/// quotes or a newline stays one line of valid syntax). A new leaf
/// variant is a compile error only in [`example_leaf_value`].
pub(crate) fn placeholder(
    shape: &Shape,
    inline: &mut dyn FnMut(&Value) -> Result<String, FormatError>,
) -> Result<String, FormatError> {
    match shape {
        Shape::Array(_) => Ok("[]".to_string()),
        Shape::Map(_) => Ok("{}".to_string()),
        Shape::Leaf(leaf) => inline(&example_leaf_value(&leaf.ty)),
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
    fn example_leaf_value_integer_respects_bounds() {
        assert_eq!(
            example_leaf_value(&LeafType::Integer {
                min: Some(5),
                max: Some(10)
            }),
            Value::Integer(5)
        );
        assert_eq!(
            example_leaf_value(&LeafType::Integer {
                min: Some(-10),
                max: Some(-1)
            }),
            Value::Integer(-1)
        );
        assert_eq!(
            example_leaf_value(&LeafType::Integer {
                min: Some(-5),
                max: Some(5)
            }),
            Value::Integer(0)
        );
    }

    #[test]
    fn example_leaf_value_enum_is_first_allowed_member() {
        assert_eq!(
            example_leaf_value(&LeafType::Enum {
                values: vec![Value::String("alpha".into()), Value::String("beta".into())]
            }),
            Value::String("alpha".into())
        );
        assert_eq!(
            example_shape_value(&Shape::from(Field::enum_of(["alpha", "beta"]))),
            Value::String("alpha".into())
        );
    }

    #[test]
    fn tagged_variant_example_materializes_nested_tagged_as_first_variant() {
        let inner = Shape::tagged("Inner", "kind")
            .variant(
                "alpha",
                Schema::object("Alpha").field("n", Field::integer()).build(),
            )
            .variant(
                "beta",
                Schema::object("Beta").field("s", Field::string()).build(),
            )
            .build();
        let outer = Shape::tagged("Outer", "mode")
            .variant(
                "wrap",
                Schema::object("Wrap")
                    .field("child", Shape::from(inner))
                    .build(),
            )
            .build();
        let example = tagged_variant_example_schema(&outer, &outer.variants[0]);
        match &example
            .fields
            .iter()
            .find(|f| f.name == "child")
            .expect("wrap has child")
            .field
        {
            Shape::Object(child) => {
                assert!(child.fields.iter().any(|f| f.name == "kind"));
                assert!(child.fields.iter().any(|f| f.name == "n"));
                assert!(!child.fields.iter().any(|f| f.name == "s"));
            }
            other => panic!("nested tagged must materialize as an object, got {other:?}"),
        }
    }

    #[test]
    fn placeholder_containers_stay_empty_and_leaves_go_through_inline() {
        let mut boom = |_: &Value| -> Result<String, FormatError> {
            unreachable!("containers do not serialize an example value")
        };
        assert_eq!(
            placeholder(
                &Shape::from(Field::array_of_type(LeafType::String)),
                &mut boom
            )
            .unwrap(),
            "[]"
        );
        assert_eq!(
            placeholder(&Shape::from(Field::map_of(LeafType::String)), &mut boom).unwrap(),
            "{}"
        );

        let mut debug = |v: &Value| Ok(format!("{v:?}"));
        assert_eq!(
            placeholder(&Shape::from(Field::string()), &mut debug).unwrap(),
            format!("{:?}", Value::String(String::new()))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::integer()), &mut debug).unwrap(),
            format!("{:?}", Value::Integer(0))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::float()), &mut debug).unwrap(),
            format!("{:?}", Value::Float(0.0))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::boolean()), &mut debug).unwrap(),
            format!("{:?}", Value::Boolean(false))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::enum_of(["alpha", "beta"])), &mut debug).unwrap(),
            format!("{:?}", Value::String("alpha".into()))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::enum_of([1.5_f64])), &mut debug).unwrap(),
            format!("{:?}", Value::Float(1.5))
        );
        assert_eq!(
            placeholder(&Shape::from(Field::enum_of([r#"a"b\c"#])), &mut debug).unwrap(),
            format!("{:?}", Value::String(r#"a"b\c"#.into()))
        );
        assert_eq!(
            placeholder(
                &Shape::from(Field::integer_in(Some(5), Some(10))),
                &mut debug
            )
            .unwrap(),
            format!("{:?}", Value::Integer(5))
        );
    }
}
