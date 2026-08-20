//! Schema-driven walks of a value [`Map`] against a [`Shape`]: the
//! resolve pipeline's validation stages as free functions.
//!
//! Document-root entry points take [`DocumentRoot`]. Object-root callers
//! still pass [`Schema`] (the [`Shape::Object`] payload); a root
//! [`Shape::Map`] treats the document table as the map itself (keys are
//! user data, not unknown). Tagged objects use a two-phase unknown-key
//! walk: pre-merge union of the tag plus every variant's fields, then
//! post-merge branch-exclusive keys against the selected variant.
//! Homogeneous maps and arrays of leaves or of objects share
//! [`Shape::Map`] / [`Shape::Array`].
//!
//! Every function here recurses a parsed table and the schema tree side
//! by side:
//!
//! - **[`collect_unknown_paths`]**: every key not declared in the schema
//!   is collected as an [`UnknownKey`]; the caller threads the list
//!   through [`crate::validate::filter_through_cascade`] for strictness
//!   decisions. One walker serves every layer — per-file tables and the
//!   env-derived table alike.
//! - **[`fill_defaults_into`]**: every missing leaf with a declared
//!   `default` is populated in place into the merged table; absent nested
//!   sections, map-of nodes, and non-optional map leaves materialize as
//!   empty tables (an absent map is the empty map), and absent array-of
//!   nodes and non-optional array leaves without a default materialize as
//!   empty arrays (an absent array is the empty array). Default origins
//!   are filled in the same walk (ADR-0004). Each fill emits a `trace`
//!   event naming the [`ConfigPath`] and value type, never the value.
//! - **[`finalize`]**: applies schema-driven coercions (string values in
//!   TOML's four datetime lexical forms become [`Value::Datetime`] on
//!   datetime leaves per ADR-0001; integer values become [`Value::Float`]
//!   on float leaves, matching what serde accepts), then recursively
//!   type-checks every value against its `LeafType`, enum-checks
//!   `LeafType::Enum`, and enforces required fields. Coercion does **not**
//!   change origin. A required miss becomes
//!   [`ClapfigError::MissingRequired`] carrying the injected discovery
//!   record (an absent key has no origin). A type/enum/shape error on a
//!   value that exists becomes [`ClapfigError::InvalidValue`] naming the
//!   winning origin. Returns the merged value [`Map`] (the typed
//!   [`TypedBuilder`](crate::TypedBuilder) deserializes that map into `C`
//!   afterwards).
//!
//! [`Schema`]: crate::runtime::Schema
//! [`DocumentRoot`]: crate::runtime::DocumentRoot
//! [`UnknownKey`]: crate::validate::UnknownKey

use crate::error::{ClapfigError, DiscoveryRecord};
use crate::format::ConfigPath;
use crate::origin::{Origin, OriginMap, OriginNode};
use crate::runtime::{DocumentRoot, NamedField, Schema, Shape, TaggedShape};
use crate::validate::UnknownKey;
use crate::value::{Map, Value};

/// Recursively walk `table` against `schema`, collecting unknown keys.
///
/// `prefix` is the dotted display form (cascade / error rendering).
/// `path` is the structured address of `table` — the same [`ConfigPath`]
/// the adapter indexed — so span lookup does not reconstruct from the
/// display string (a quoted dotted map-entry key stays one segment).
///
/// For nested objects (`Shape::Object`) the recursion descends into the
/// sub-table; for `Shape::Array`, each entry is validated against the
/// item shape (with an `[index]` path segment); for `Shape::Map`,
/// each entry's value is validated against the item shape (with the
/// user-supplied key forming a path segment).
///
/// The same walker serves the per-file pass and the env layer. Env
/// dotted-key syntax cannot express arrays-of-tables, so the
/// `Shape::Array` arm simply never fires there — recursion support is
/// harmless.
pub(crate) fn collect_unknown_paths(
    table: &Map,
    schema: &Schema,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let child = path.clone().key(key);
        match find_field(schema, key) {
            None => {
                // Capture the raw map key as the leaf — preserves quoted-
                // key semantics (`"acme.task-due-date-missing"` stays as a
                // single literal) so an `on_unknown_key` callback can
                // pattern-match on it (e.g. lex-fmt's "leaf contains a `.`
                // → accept" rule).
                unknown.push(UnknownKey {
                    path: full,
                    leaf: key.clone(),
                    config_path: child,
                });
            }
            Some(nf) => collect_unknown_against_shape(value, &nf.field, &full, &child, unknown),
        }
    }
}

/// Recurse unknown-key collection into a field's [`Shape`].
fn collect_unknown_against_shape(
    value: &Value,
    shape: &Shape,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    match shape {
        Shape::Leaf(_) => {
            // Leaf — type checking happens later in `finalize`.
        }
        Shape::Object(nested) => {
            if let Value::Map(t) = value {
                collect_unknown_paths(t, nested, prefix, path, unknown);
            }
        }
        Shape::Array(array) => {
            if let Value::Array(items) = value {
                for (i, item) in items.iter().enumerate() {
                    let indexed = format!("{prefix}[{i}]");
                    collect_unknown_against_shape(
                        item,
                        &array.item,
                        &indexed,
                        &path.clone().index(i),
                        unknown,
                    );
                }
            }
        }
        Shape::Map(map) => {
            // Map keys are user data, not unknown. Recurse into each
            // entry's value against the item shape. A quoted dotted
            // entry key is one [`ConfigPath`] segment even though
            // `prefix` flattens it.
            if let Value::Map(entries) = value {
                for (entry_key, entry_value) in entries {
                    let entry_path = format!("{prefix}.{entry_key}");
                    collect_unknown_against_shape(
                        entry_value,
                        &map.item,
                        &entry_path,
                        &path.clone().key(entry_key),
                        unknown,
                    );
                }
            }
        }
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value {
                collect_unknown_tagged_phase1(inner, tagged, prefix, path, unknown);
            }
        }
    }
}

/// Walk `table` against a document root. Object roots use the named-field
/// walker; tagged roots run phase 1 (union of tag + every variant field).
pub(crate) fn collect_unknown_paths_root(
    table: &Map,
    root: DocumentRoot<'_>,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    match root {
        DocumentRoot::Object(schema) => collect_unknown_paths(table, schema, prefix, path, unknown),
        DocumentRoot::Map(map) => {
            for (entry_key, entry_value) in table {
                collect_unknown_against_shape(
                    entry_value,
                    &map.item,
                    entry_key,
                    &path.clone().key(entry_key),
                    unknown,
                );
            }
        }
        DocumentRoot::Tagged(tagged) => {
            collect_unknown_tagged_phase1(table, tagged, prefix, path, unknown)
        }
    }
}

/// Phase 1: a key is known if it is the tag or a field of *any* variant.
/// True unknowns are collected here; branch-exclusive keys are not.
fn collect_unknown_tagged_phase1(
    table: &Map,
    tagged: &TaggedShape,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    for (key, value) in table {
        let full = join_prefix(prefix, key);
        let child = path.clone().key(key);
        if key == &tagged.tag {
            continue;
        }
        let shapes = tagged.field_shapes(key);
        if shapes.is_empty() {
            unknown.push(UnknownKey {
                path: full,
                leaf: key.clone(),
                config_path: child,
            });
        } else {
            collect_unknown_against_shapes_union(value, &shapes, &full, &child, unknown);
        }
    }
}

/// Recurse unknown-key collection through every shape a name is declared
/// as across variants, so a nested key valid for any variant is known at
/// phase 1. Mixed constructors on the same field (Object in one variant,
/// Map / Tagged / `Value` in another) combine: a key is unknown only if
/// every alternative treats it as unknown. A leaf that accepts the whole
/// encountered value (`LeafType::Value`, or any leaf whose type-check
/// succeeds) terminates that union: nested entries are not re-checked
/// against structured alternatives.
fn collect_unknown_against_shapes_union(
    value: &Value,
    shapes: &[&Shape],
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    if shapes.iter().any(|s| leaf_accepts(s, value)) {
        return;
    }
    match value {
        Value::Map(inner) => {
            collect_unknown_union_map(inner, shapes, prefix, path, unknown);
        }
        Value::Array(items) => {
            let array_items: Vec<&Shape> = shapes
                .iter()
                .filter_map(|s| match s {
                    Shape::Array(array) => Some(array.item.as_ref()),
                    _ => None,
                })
                .collect();
            if array_items.is_empty() {
                return;
            }
            for (i, item) in items.iter().enumerate() {
                collect_unknown_against_shapes_union(
                    item,
                    &array_items,
                    &format!("{prefix}[{i}]"),
                    &path.clone().index(i),
                    unknown,
                );
            }
        }
        _ => {}
    }
}

fn leaf_accepts(shape: &Shape, value: &Value) -> bool {
    match shape {
        Shape::Leaf(leaf) => leaf.ty.check(value).is_ok(),
        _ => false,
    }
}

fn collect_unknown_union_map(
    table: &Map,
    shapes: &[&Shape],
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    for (key, value) in table {
        let full = join_prefix(prefix, key);
        let child = path.clone().key(key);
        let mut nested: Vec<&Shape> = Vec::new();
        let mut known = false;
        for shape in shapes {
            match shape {
                Shape::Leaf(_) => known = true,
                Shape::Map(map) => {
                    known = true;
                    nested.push(map.item.as_ref());
                }
                Shape::Object(schema) => {
                    if let Some(nf) = find_field(schema, key) {
                        known = true;
                        nested.push(&nf.field);
                    }
                }
                Shape::Tagged(tagged) => {
                    if key == tagged.tag.as_str() {
                        known = true;
                    } else {
                        let field_shapes = tagged.field_shapes(key);
                        if !field_shapes.is_empty() {
                            known = true;
                            nested.extend(field_shapes);
                        }
                    }
                }
                Shape::Array(_) => {}
            }
        }
        if !known {
            unknown.push(UnknownKey {
                path: full,
                leaf: key.clone(),
                config_path: child,
            });
        } else if !nested.is_empty() {
            collect_unknown_against_shapes_union(value, &nested, &full, &child, unknown);
        }
    }
}

fn join_prefix(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Phase 2: after merge and branch selection, collect keys that belong
/// to some other variant but not the selected one (and are not the tag).
/// True unknowns are not candidates.
pub(crate) fn collect_branch_exclusive_root(
    table: &Map,
    root: DocumentRoot<'_>,
    unknown: &mut Vec<UnknownKey>,
) {
    match root {
        DocumentRoot::Object(schema) => {
            collect_branch_exclusive_object(table, schema, "", &ConfigPath::new(), unknown)
        }
        DocumentRoot::Map(map) => {
            let path = ConfigPath::new();
            for (entry_key, entry_value) in table {
                collect_branch_exclusive_in_shape(
                    entry_value,
                    &map.item,
                    entry_key,
                    &path.clone().key(entry_key),
                    unknown,
                );
            }
        }
        DocumentRoot::Tagged(tagged) => {
            collect_branch_exclusive_tagged(table, tagged, "", &ConfigPath::new(), unknown, &[])
        }
    }
}

fn collect_branch_exclusive_object(
    table: &Map,
    schema: &Schema,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    for nf in &schema.fields {
        if let Some(value) = table.get(&nf.name) {
            collect_branch_exclusive_in_shape(
                value,
                &nf.field,
                &join_prefix(prefix, &nf.name),
                &path.clone().key(&nf.name),
                unknown,
            );
        }
    }
}

fn collect_branch_exclusive_in_shape(
    value: &Value,
    shape: &Shape,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    match shape {
        Shape::Leaf(_) => {}
        Shape::Object(nested) => {
            if let Value::Map(inner) = value {
                collect_branch_exclusive_object(inner, nested, prefix, path, unknown);
            }
        }
        Shape::Array(array) => {
            if let Value::Array(items) = value {
                for (i, item) in items.iter().enumerate() {
                    collect_branch_exclusive_in_shape(
                        item,
                        &array.item,
                        &format!("{prefix}[{i}]"),
                        &path.clone().index(i),
                        unknown,
                    );
                }
            }
        }
        Shape::Map(map) => {
            if let Value::Map(entries) = value {
                for (entry_key, entry_value) in entries {
                    collect_branch_exclusive_in_shape(
                        entry_value,
                        &map.item,
                        &format!("{prefix}.{entry_key}"),
                        &path.clone().key(entry_key),
                        unknown,
                    );
                }
            }
        }
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value {
                collect_branch_exclusive_tagged(inner, tagged, prefix, path, unknown, &[]);
            }
        }
    }
}

/// Phase-2 exclusive keys for a tagged object. `others` are sibling
/// constructors for the same field from a parent union (empty at a
/// tagged document root). Intra-union other-variant fields and those
/// sibling constructors are combined in one walk so nested exclusive
/// keys are reported once.
fn collect_branch_exclusive_tagged(
    table: &Map,
    tagged: &TaggedShape,
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
    others: &[&Shape],
) {
    let Some(selected) = tagged.selected(table) else {
        return;
    };
    let other_fields: Vec<(&str, &Shape)> = tagged
        .variants
        .iter()
        .filter(|v| v.discriminator != selected.discriminator)
        .flat_map(|v| v.schema.fields.iter().map(|f| (f.name.as_str(), &f.field)))
        .collect();
    for (key, value) in table {
        if key == &tagged.tag {
            continue;
        }
        let full = join_prefix(prefix, key);
        let child = path.clone().key(key);
        if let Some(nf) = find_field(&selected.schema, key) {
            let mut nested_others: Vec<&Shape> = other_fields
                .iter()
                .filter(|(name, _)| *name == key.as_str())
                .map(|(_, shape)| *shape)
                .collect();
            for other in others {
                if let Some(ns) = nested_shapes_for_key(other, key) {
                    nested_others.extend(ns);
                }
            }
            collect_exclusive_against_shape(
                value,
                &nf.field,
                &nested_others,
                &full,
                &child,
                unknown,
            );
        } else if other_fields.iter().any(|(name, _)| *name == key.as_str())
            || others
                .iter()
                .any(|s| nested_shapes_for_key(s, key).is_some())
        {
            unknown.push(UnknownKey {
                path: full,
                leaf: key.clone(),
                config_path: child,
            });
        }
    }
}

fn collect_exclusive_against_shape(
    value: &Value,
    selected: &Shape,
    others: &[&Shape],
    prefix: &str,
    path: &ConfigPath,
    unknown: &mut Vec<UnknownKey>,
) {
    match selected {
        Shape::Leaf(_) => {}
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value {
                collect_branch_exclusive_tagged(inner, tagged, prefix, path, unknown, others);
            }
        }
        Shape::Array(array) => {
            let other_items: Vec<&Shape> = others
                .iter()
                .filter_map(|s| match s {
                    Shape::Array(a) => Some(a.item.as_ref()),
                    _ => None,
                })
                .collect();
            if let Value::Array(items) = value {
                for (i, item) in items.iter().enumerate() {
                    collect_exclusive_against_shape(
                        item,
                        &array.item,
                        &other_items,
                        &format!("{prefix}[{i}]"),
                        &path.clone().index(i),
                        unknown,
                    );
                }
            }
        }
        Shape::Object(_) | Shape::Map(_) => {
            let Value::Map(inner) = value else {
                return;
            };
            for (key, nested) in inner {
                let full = join_prefix(prefix, key);
                let child = path.clone().key(key);
                match nested_shapes_for_key(selected, key) {
                    Some(sel_nested) if sel_nested.is_empty() => {}
                    Some(sel_nested) => {
                        let mut other_nested = Vec::new();
                        for other in others {
                            if let Some(ns) = nested_shapes_for_key(other, key) {
                                other_nested.extend(ns);
                            }
                        }
                        for sel in sel_nested {
                            collect_exclusive_against_shape(
                                nested,
                                sel,
                                &other_nested,
                                &full,
                                &child,
                                unknown,
                            );
                        }
                    }
                    None if others
                        .iter()
                        .any(|s| nested_shapes_for_key(s, key).is_some()) =>
                    {
                        unknown.push(UnknownKey {
                            path: full,
                            leaf: key.clone(),
                            config_path: child,
                        });
                    }
                    None => {}
                }
            }
        }
    }
}

/// Nested shapes `shape` imposes on map key `key`.
///
/// `None` — this constructor does not know the key (candidate exclusive
/// against another variant). `Some([])` — known as a leaf (tag, `Value`,
/// scalar); no further unknown-key walk. `Some(shapes)` — known, recurse.
fn nested_shapes_for_key<'a>(shape: &'a Shape, key: &str) -> Option<Vec<&'a Shape>> {
    match shape {
        Shape::Object(schema) => find_field(schema, key).map(|nf| vec![&nf.field]),
        Shape::Tagged(tagged) => {
            if key == tagged.tag.as_str() {
                Some(Vec::new())
            } else {
                let field_shapes = tagged.field_shapes(key);
                if field_shapes.is_empty() {
                    None
                } else {
                    Some(field_shapes)
                }
            }
        }
        Shape::Map(map) => Some(vec![map.item.as_ref()]),
        Shape::Leaf(_) => Some(Vec::new()),
        Shape::Array(_) => None,
    }
}

fn find_field<'a>(schema: &'a Schema, name: &str) -> Option<&'a NamedField> {
    schema.fields.iter().find(|f| f.name == name)
}

/// Recursively populate missing leaves in `table` with their schema-declared
/// defaults. Absent `Nested` sections, absent `MapOf` nodes, and absent
/// non-optional map-typed leaves materialize as empty tables (an absent
/// section/map is the empty one — and the typed path's serde deserialize
/// needs the table present); absent `ArrayOf` nodes and absent non-optional
/// array-typed leaves without a declared default materialize as empty
/// arrays by the same rule (an absent array is the empty array). Existing
/// values are never overwritten.
///
/// Default origins are filled in the same walk (ADR-0004): a newly
/// inserted default, empty map, or empty array gets [`Origin::default`]
/// naming the schema key; existing origin nodes are left in place.
/// Each fill emits a `trace` event with the [`ConfigPath`] and value
/// **type** (never the value); a `debug` summary reports how many were
/// filled. Path tracking for those events is skipped when TRACE is
/// disabled.
pub(crate) fn fill_defaults_into(table: &mut Map, origins: &mut OriginMap, schema: &Schema) {
    let mut filled = 0usize;
    let path = crate::trace::trace_event_enabled().then(ConfigPath::new);
    fill_defaults_at(table, origins, schema, "", path, &mut filled);
    crate::trace::defaults_filled(filled);
}

fn fill_defaults_at(
    table: &mut Map,
    origins: &mut OriginMap,
    schema: &Schema,
    prefix: &str,
    path: Option<ConfigPath>,
    filled: &mut usize,
) {
    for nf in &schema.fields {
        let schema_key = if prefix.is_empty() {
            nf.name.clone()
        } else {
            format!("{prefix}.{}", nf.name)
        };
        let child = path.as_ref().map(|p| p.clone().key(&nf.name));
        fill_defaults_for_field(
            table,
            origins,
            &nf.name,
            &nf.field,
            &schema_key,
            child,
            filled,
        );
    }
}

fn fill_defaults_for_field(
    table: &mut Map,
    origins: &mut OriginMap,
    name: &str,
    shape: &Shape,
    schema_key: &str,
    child: Option<ConfigPath>,
    filled: &mut usize,
) {
    match shape {
        Shape::Leaf(leaf) => {
            if !table.contains_key(name)
                && let Some(default) = &leaf.default
            {
                insert_default(
                    table,
                    origins,
                    name,
                    default.clone(),
                    child.as_ref(),
                    schema_key,
                    filled,
                );
            }
        }
        Shape::Object(nested) => {
            let created = !table.contains_key(name);
            let entry = table
                .entry(name.to_string())
                .or_insert_with(|| Value::Map(Map::new()));
            if created {
                origins.entry(name.to_string()).or_insert_with(|| {
                    OriginNode::map(Origin::default(schema_key), OriginMap::new())
                });
                trace_default_filled(child.as_ref(), "map", filled);
            }
            if let Value::Map(t) = entry {
                let child_origins = child_map_origins(origins, name, schema_key);
                fill_defaults_at(t, child_origins, nested, schema_key, child, filled);
            }
        }
        Shape::Array(array) => {
            // Array entries are user-supplied — push defaults into
            // existing entries, never synthesize missing array items.
            // An absent non-optional array without a declared default
            // materializes as the empty array. Optional arrays
            // (`Option<Vec<..>>`) stay absent and deserialize to `None`.
            // After inserting a declared default, walk the inserted
            // value so nested object defaults still fill.
            if !table.contains_key(name) {
                if let Some(default) = &array.default {
                    insert_default(
                        table,
                        origins,
                        name,
                        default.clone(),
                        child.as_ref(),
                        schema_key,
                        filled,
                    );
                } else if array.optional {
                    return;
                }
            }
            let created = !table.contains_key(name);
            table
                .entry(name.to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if created {
                origins
                    .entry(name.to_string())
                    .or_insert_with(|| OriginNode::array(Origin::default(schema_key), Vec::new()));
                trace_default_filled(child.as_ref(), "array", filled);
            }
            if let Some(entry) = table.get_mut(name) {
                if !origins.contains_key(name) {
                    origins.insert(
                        name.to_string(),
                        OriginNode::from_value(entry, Origin::default(schema_key)),
                    );
                }
                let node = origins.get_mut(name).expect("just ensured origin node");
                fill_defaults_in_value(entry, node, shape, schema_key, child, filled);
            }
        }
        Shape::Map(map) => {
            // Map entries are user-supplied — push defaults into
            // existing entries, never synthesize missing entries. An
            // absent non-optional map without a declared default
            // materializes as the empty map. Optional maps
            // (`Option<Map<..>>`) stay absent and deserialize to `None`.
            if !table.contains_key(name) {
                if let Some(default) = &map.default {
                    insert_default(
                        table,
                        origins,
                        name,
                        default.clone(),
                        child.as_ref(),
                        schema_key,
                        filled,
                    );
                } else if map.optional {
                    return;
                }
            }
            let created = !table.contains_key(name);
            table
                .entry(name.to_string())
                .or_insert_with(|| Value::Map(Map::new()));
            if created {
                origins.entry(name.to_string()).or_insert_with(|| {
                    OriginNode::map(Origin::default(schema_key), OriginMap::new())
                });
                trace_default_filled(child.as_ref(), "map", filled);
            }
            if let Some(entry) = table.get_mut(name) {
                if !origins.contains_key(name) {
                    origins.insert(
                        name.to_string(),
                        OriginNode::from_value(entry, Origin::default(schema_key)),
                    );
                }
                let node = origins.get_mut(name).expect("just ensured origin node");
                fill_defaults_in_value(entry, node, shape, schema_key, child, filled);
            }
        }
        Shape::Tagged(tagged) => {
            // An absent tagged object materializes as the empty table
            // (same absence rule as a nested object); MissingRequired on
            // the tag fires in finalize. Defaults fill only the selected
            // variant, after the discriminator is present.
            let created = !table.contains_key(name);
            table
                .entry(name.to_string())
                .or_insert_with(|| Value::Map(Map::new()));
            if created {
                origins.entry(name.to_string()).or_insert_with(|| {
                    OriginNode::map(Origin::default(schema_key), OriginMap::new())
                });
                trace_default_filled(child.as_ref(), "map", filled);
            }
            if let Some(Value::Map(inner)) = table.get_mut(name) {
                let child_origins = child_map_origins(origins, name, schema_key);
                fill_defaults_tagged(inner, child_origins, tagged, schema_key, child, filled);
            }
        }
    }
}

/// Recursively fill defaults in an existing `value` against `shape`.
/// Object fields use [`fill_defaults_at`]; Array and Map walk existing
/// entries (never synthesizing missing items) so nested objects under
/// multiple container layers still receive their declared defaults.
fn fill_defaults_in_value(
    value: &mut Value,
    origin: &mut OriginNode,
    shape: &Shape,
    schema_key: &str,
    path: Option<ConfigPath>,
    filled: &mut usize,
) {
    match shape {
        Shape::Leaf(_) => {}
        Shape::Object(nested) => {
            if let Value::Map(t) = value {
                fill_defaults_at(
                    t,
                    origin.map_children_mut(),
                    nested,
                    schema_key,
                    path,
                    filled,
                );
            }
        }
        Shape::Array(array) => {
            if let Value::Array(items) = value {
                let children = origin.array_children_mut();
                for i in 0..items.len() {
                    let indexed = format!("{schema_key}[{i}]");
                    let indexed_path = path.as_ref().map(|p| p.clone().index(i));
                    while children.len() <= i {
                        children.push(OriginNode::from_value(&items[i], Origin::default(&indexed)));
                    }
                    fill_defaults_in_value(
                        &mut items[i],
                        &mut children[i],
                        &array.item,
                        &indexed,
                        indexed_path,
                        filled,
                    );
                }
            }
        }
        Shape::Map(map) => {
            if let Value::Map(entries) = value {
                let children = origin.map_children_mut();
                let keys: Vec<String> = entries.keys().cloned().collect();
                for key in keys {
                    let entry_path = format!("{schema_key}.{key}");
                    let entry_cfg = path.as_ref().map(|p| p.clone().key(&key));
                    if !children.contains_key(&key) {
                        children.insert(
                            key.clone(),
                            OriginNode::from_value(
                                entries.get(&key).expect("key came from entries"),
                                Origin::default(&entry_path),
                            ),
                        );
                    }
                    let entry_value = entries.get_mut(&key).expect("key came from entries");
                    let node = children.get_mut(&key).expect("just ensured origin node");
                    fill_defaults_in_value(
                        entry_value,
                        node,
                        &map.item,
                        &entry_path,
                        entry_cfg,
                        filled,
                    );
                }
            }
        }
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value {
                fill_defaults_tagged(
                    inner,
                    origin.map_children_mut(),
                    tagged,
                    schema_key,
                    path,
                    filled,
                );
            }
        }
    }
}

/// Fill defaults for a tagged object: only the selected variant's fields,
/// and only when the discriminator already names a known variant.
fn fill_defaults_tagged(
    table: &mut Map,
    origins: &mut OriginMap,
    tagged: &TaggedShape,
    prefix: &str,
    path: Option<ConfigPath>,
    filled: &mut usize,
) {
    if let Some(variant) = tagged.selected(table) {
        fill_defaults_at(table, origins, &variant.schema, prefix, path, filled);
    }
}

fn fill_defaults_in_root_map(
    table: &mut Map,
    origins: &mut OriginMap,
    map: &crate::runtime::MapShape,
    path: Option<ConfigPath>,
    filled: &mut usize,
) {
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        let entry_cfg = path.as_ref().map(|p| p.clone().key(&key));
        if let Some(entry) = table.get_mut(&key) {
            if !origins.contains_key(&key) {
                origins.insert(
                    key.clone(),
                    OriginNode::from_value(entry, Origin::default(&key)),
                );
            }
            let node = origins.get_mut(&key).expect("just ensured origin node");
            fill_defaults_in_value(entry, node, &map.item, &key, entry_cfg, filled);
        }
    }
}

/// Populate defaults against a document root (object, map, or tagged).
pub(crate) fn fill_defaults_into_root(
    table: &mut Map,
    origins: &mut OriginMap,
    root: DocumentRoot<'_>,
) {
    match root {
        DocumentRoot::Object(schema) => fill_defaults_into(table, origins, schema),
        DocumentRoot::Map(map) => {
            let mut filled = 0usize;
            let path = crate::trace::trace_event_enabled().then(ConfigPath::new);
            fill_defaults_in_root_map(table, origins, map, path, &mut filled);
            crate::trace::defaults_filled(filled);
        }
        DocumentRoot::Tagged(tagged) => {
            let mut filled = 0usize;
            let path = crate::trace::trace_event_enabled().then(ConfigPath::new);
            fill_defaults_tagged(table, origins, tagged, "", path, &mut filled);
            crate::trace::defaults_filled(filled);
        }
    }
}

fn trace_default_filled(key: Option<&ConfigPath>, value_type: &str, filled: &mut usize) {
    *filled += 1;
    if let Some(key) = key {
        crate::trace::default_filled(key, value_type);
    }
}

fn insert_default(
    table: &mut Map,
    origins: &mut OriginMap,
    name: &str,
    value: Value,
    key: Option<&ConfigPath>,
    schema_key: &str,
    filled: &mut usize,
) {
    trace_default_filled(key, value.type_str(), filled);
    origins.insert(
        name.to_string(),
        OriginNode::from_value(&value, Origin::default(schema_key)),
    );
    table.insert(name.to_string(), value);
}

fn child_map_origins<'a>(
    origins: &'a mut OriginMap,
    name: &str,
    schema_key: &str,
) -> &'a mut OriginMap {
    let node = origins
        .entry(name.to_string())
        .or_insert_with(|| OriginNode::map(Origin::default(schema_key), OriginMap::new()));
    node.map_children_mut()
}

/// Finalize a merged table: apply schema-driven coercions
/// (datetime strings, integer-for-float), then enforce required fields
/// and per-leaf types. Returns the merged map on success.
///
/// Coercion changes a value's type (datetime strings, integer-for-float)
/// and does **not** change origin: the winning input still owns the
/// value.
///
/// `origins` is the lockstep shadow tree; type/enum/shape errors on a
/// value that exists name the origin at that path.
/// `discovery` is attached to [`ClapfigError::MissingRequired`] — an
/// absent key has no origin, it has the search that did not find it.
/// Callers without a probe record (schema-only unit tests) pass
/// [`DiscoveryRecord::empty`].
pub(crate) fn finalize(
    mut merged: Map,
    origins: &OriginMap,
    schema: &Schema,
    discovery: &DiscoveryRecord,
) -> Result<Map, ClapfigError> {
    coerce_leaf_values(&mut merged, schema);
    check_required_and_types(&merged, origins, schema, "", &ConfigPath::new(), discovery)?;
    Ok(merged)
}

/// Schema-driven value coercion: for every leaf the schema declares
/// [`LeafType::DateTime`](crate::runtime::LeafType::DateTime), a merged
/// string value that parses as one of TOML's four datetime lexical forms
/// is replaced in place with the typed [`Value::Datetime`] (ADR-0001);
/// for every [`LeafType::Float`](crate::runtime::LeafType::Float) leaf, a
/// merged integer value is replaced with the equivalent
/// [`Value::Float`] (serde accepts integers for `f64` fields, so the
/// value model does too). Values matching neither rule are left
/// untouched, so the type check that follows reports the normal
/// "expected …, got …" error.
///
/// This is the seam that lets schema-blind sources — YAML/JSON files, env
/// vars, CLI/URL overrides — express datetimes as strings and floats as
/// bare integers. Detection is never value-sniffing: only the declared
/// leaf type makes a value a candidate.
fn coerce_leaf_values(table: &mut Map, schema: &Schema) {
    for nf in &schema.fields {
        if let Some(value) = table.get_mut(&nf.name) {
            coerce_value(value, &nf.field);
        }
    }
}

/// Coerce one value against its declared shape (datetime strings on
/// datetime leaves, integers on float leaves), recursing through
/// declared containers. Shared with the persist path, which validates
/// `config set` values against the same declarations.
pub(crate) fn coerce_value(value: &mut Value, shape: &Shape) {
    match shape {
        Shape::Leaf(leaf) => coerce_leaf(value, &leaf.ty),
        Shape::Object(nested) => {
            if let Value::Map(t) = value {
                coerce_leaf_values(t, nested);
            }
        }
        Shape::Array(array) => {
            if let Value::Array(items) = value {
                for item in items {
                    coerce_value(item, &array.item);
                }
            }
        }
        Shape::Map(map) => {
            if let Value::Map(entries) = value {
                for entry in entries.values_mut() {
                    coerce_value(entry, &map.item);
                }
            }
        }
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value
                && let Some(variant) = tagged.selected(inner)
            {
                coerce_leaf_values(inner, &variant.schema);
            }
        }
    }
}

fn coerce_leaf(value: &mut Value, ty: &crate::runtime::LeafType) {
    use crate::runtime::LeafType;
    match ty {
        LeafType::DateTime => {
            if let Value::String(s) = value
                && let Ok(dt) = s.parse::<crate::value::Datetime>()
            {
                *value = Value::Datetime(dt);
            }
        }
        LeafType::Float => {
            if let Value::Integer(i) = value {
                *value = Value::Float(*i as f64);
            }
        }
        _ => {}
    }
}

/// Recursively validate required-field presence and per-leaf types.
fn check_required_and_types(
    table: &Map,
    origins: &OriginMap,
    schema: &Schema,
    prefix: &str,
    path: &ConfigPath,
    discovery: &DiscoveryRecord,
) -> Result<(), ClapfigError> {
    for nf in &schema.fields {
        let display = if prefix.is_empty() {
            nf.name.clone()
        } else {
            format!("{prefix}.{}", nf.name)
        };
        let child = path.clone().key(&nf.name);
        check_field(
            table.get(&nf.name),
            origins,
            &nf.field,
            &display,
            &child,
            discovery,
        )?;
    }
    Ok(())
}

fn check_field(
    value: Option<&Value>,
    origins: &OriginMap,
    shape: &Shape,
    display: &str,
    path: &ConfigPath,
    discovery: &DiscoveryRecord,
) -> Result<(), ClapfigError> {
    match shape {
        Shape::Leaf(leaf) => match value {
            None => {
                if !leaf.optional {
                    return Err(ClapfigError::missing_required(
                        display.to_string(),
                        discovery.clone(),
                    ));
                }
                Ok(())
            }
            Some(value) => leaf.ty.check(value).map_err(|reason| {
                ClapfigError::invalid_value_at(display.to_string(), reason, origins, path)
            }),
        },
        Shape::Object(nested) => match value {
            None => {
                // A nested section is required if any of its leaves is
                // required. Recurse with an empty table so the missing-
                // required check below fires for inner leaves.
                let empty = Map::new();
                check_required_and_types(
                    &empty,
                    &OriginMap::new(),
                    nested,
                    display,
                    path,
                    discovery,
                )
            }
            Some(Value::Map(inner)) => {
                check_required_and_types(inner, origins, nested, display, path, discovery)
            }
            Some(other) => Err(ClapfigError::invalid_value_at(
                display.to_string(),
                format!("expected map, got {}", value_type_name(other)),
                origins,
                path,
            )),
        },
        Shape::Array(array) => match value {
            None => Ok(()),
            Some(value) if array.item.is_value_field() => {
                shape.check_value(value).map_err(|reason| {
                    ClapfigError::invalid_value_at(display.to_string(), reason, origins, path)
                })
            }
            Some(Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    let indexed = format!("{display}[{i}]");
                    let indexed_path = path.clone().index(i);
                    check_field(
                        Some(item),
                        origins,
                        &array.item,
                        &indexed,
                        &indexed_path,
                        discovery,
                    )?;
                }
                Ok(())
            }
            Some(other) => Err(ClapfigError::invalid_value_at(
                display.to_string(),
                format!("expected array, got {}", value_type_name(other)),
                origins,
                path,
            )),
        },
        Shape::Map(map) => match value {
            None => Ok(()),
            Some(value) if map.item.is_value_field() => {
                shape.check_value(value).map_err(|reason| {
                    ClapfigError::invalid_value_at(display.to_string(), reason, origins, path)
                })
            }
            Some(Value::Map(entries)) => {
                for (entry_key, entry_value) in entries {
                    let entry_path = format!("{display}.{entry_key}");
                    let entry_cfg = path.clone().key(entry_key);
                    check_field(
                        Some(entry_value),
                        origins,
                        &map.item,
                        &entry_path,
                        &entry_cfg,
                        discovery,
                    )?;
                }
                Ok(())
            }
            Some(other) => Err(ClapfigError::invalid_value_at(
                display.to_string(),
                format!("expected map, got {}", value_type_name(other)),
                origins,
                path,
            )),
        },
        Shape::Tagged(tagged) => match value {
            None => {
                // Absent tagged object → MissingRequired on the tag
                // (discovery, no origin). fill_defaults materializes an
                // empty table on the load path; schema-only tests may
                // still arrive here with None.
                Err(ClapfigError::missing_required(
                    tag_display(display, &tagged.tag),
                    discovery.clone(),
                ))
            }
            Some(Value::Map(inner)) => {
                check_tagged(inner, origins, tagged, display, path, discovery)
            }
            Some(other) => Err(ClapfigError::invalid_value_at(
                display.to_string(),
                format!("expected map, got {}", value_type_name(other)),
                origins,
                path,
            )),
        },
    }
}

fn tag_display(object_display: &str, tag: &str) -> String {
    if object_display.is_empty() {
        tag.to_string()
    } else {
        format!("{object_display}.{tag}")
    }
}

/// Post-merge branch selection + selected-variant required/type checks.
fn check_tagged(
    table: &Map,
    origins: &OriginMap,
    tagged: &TaggedShape,
    object_display: &str,
    path: &ConfigPath,
    discovery: &DiscoveryRecord,
) -> Result<(), ClapfigError> {
    let tag_key = tag_display(object_display, &tagged.tag);
    let tag_path = path.clone().key(&tagged.tag);
    match table.get(&tagged.tag) {
        None => Err(ClapfigError::missing_required(tag_key, discovery.clone())),
        Some(value) => {
            tagged
                .discriminator_leaf_type()
                .check(value)
                .map_err(|reason| {
                    ClapfigError::invalid_value_at(tag_key.clone(), reason, origins, &tag_path)
                })?;
            let disc = value
                .as_str()
                .expect("enum check passed: discriminator is a string in the allowed set");
            let variant = tagged
                .variant(disc)
                .expect("enum check passed: discriminator names a variant");
            check_required_and_types(
                table,
                origins,
                &variant.schema,
                object_display,
                path,
                discovery,
            )
        }
    }
}

/// Emit `tagged branch selected` for every tagged node whose discriminator
/// already names a known variant. Missing, mistyped, or unknown tags emit
/// nothing. Called after merge/defaults and before phase-2 exclusive-key
/// filtering so a rejected exclusive key still records that selection
/// happened. Nested tagged objects, array items, and map entries are
/// walked the same way [`check_tagged`] would on the success path.
pub(crate) fn trace_selected_tagged_root(table: &Map, root: DocumentRoot<'_>, origins: &OriginMap) {
    match root {
        DocumentRoot::Object(schema) => {
            trace_selected_tagged_object(table, schema, &ConfigPath::new(), origins);
        }
        DocumentRoot::Map(map) => {
            let path = ConfigPath::new();
            for (key, value) in table {
                trace_selected_tagged_in_shape(value, &map.item, &path.clone().key(key), origins);
            }
        }
        DocumentRoot::Tagged(tagged) => {
            trace_selected_tagged(table, tagged, &ConfigPath::new(), origins);
        }
    }
}

fn emit_tagged_branch_selected(
    table: &Map,
    tagged: &TaggedShape,
    path: &ConfigPath,
    origins: &OriginMap,
) {
    let Some(value) = table.get(&tagged.tag) else {
        return;
    };
    if tagged.selected(table).is_none() {
        return;
    }
    let tag_path = path.clone().key(&tagged.tag);
    match crate::origin::lookup(origins, &tag_path) {
        Some(origin) => {
            crate::trace::tagged_branch_selected(&tag_path, &origin.label(), value.type_str());
        }
        None => crate::trace::tagged_branch_selected(&tag_path, "unknown", value.type_str()),
    }
}

fn trace_selected_tagged(
    table: &Map,
    tagged: &TaggedShape,
    path: &ConfigPath,
    origins: &OriginMap,
) {
    emit_tagged_branch_selected(table, tagged, path, origins);
    let Some(selected) = tagged.selected(table) else {
        return;
    };
    for nf in &selected.schema.fields {
        if let Some(value) = table.get(&nf.name) {
            trace_selected_tagged_in_shape(value, &nf.field, &path.clone().key(&nf.name), origins);
        }
    }
}

fn trace_selected_tagged_object(
    table: &Map,
    schema: &Schema,
    path: &ConfigPath,
    origins: &OriginMap,
) {
    for nf in &schema.fields {
        if let Some(value) = table.get(&nf.name) {
            trace_selected_tagged_in_shape(value, &nf.field, &path.clone().key(&nf.name), origins);
        }
    }
}

fn trace_selected_tagged_in_shape(
    value: &Value,
    shape: &Shape,
    path: &ConfigPath,
    origins: &OriginMap,
) {
    match shape {
        Shape::Leaf(_) => {}
        Shape::Object(nested) => {
            if let Value::Map(inner) = value {
                trace_selected_tagged_object(inner, nested, path, origins);
            }
        }
        Shape::Array(array) => {
            if let Value::Array(items) = value {
                for (i, item) in items.iter().enumerate() {
                    trace_selected_tagged_in_shape(
                        item,
                        &array.item,
                        &path.clone().index(i),
                        origins,
                    );
                }
            }
        }
        Shape::Map(map) => {
            if let Value::Map(entries) = value {
                for (key, entry) in entries {
                    trace_selected_tagged_in_shape(
                        entry,
                        &map.item,
                        &path.clone().key(key),
                        origins,
                    );
                }
            }
        }
        Shape::Tagged(tagged) => {
            if let Value::Map(inner) = value {
                trace_selected_tagged(inner, tagged, path, origins);
            }
        }
    }
}

/// Finalize a merged table against a document root.
pub(crate) fn finalize_root(
    mut merged: Map,
    origins: &OriginMap,
    root: DocumentRoot<'_>,
    discovery: &DiscoveryRecord,
) -> Result<Map, ClapfigError> {
    match root {
        DocumentRoot::Object(schema) => finalize(merged, origins, schema, discovery),
        DocumentRoot::Map(map) => {
            for entry in merged.values_mut() {
                coerce_value(entry, &map.item);
            }
            let path = ConfigPath::new();
            for (entry_key, entry_value) in &merged {
                check_field(
                    Some(entry_value),
                    origins,
                    &map.item,
                    entry_key,
                    &path.clone().key(entry_key),
                    discovery,
                )?;
            }
            Ok(merged)
        }
        DocumentRoot::Tagged(tagged) => {
            if let Some(variant) = tagged.selected(&merged) {
                coerce_leaf_values(&mut merged, &variant.schema);
            }
            check_tagged(&merged, origins, tagged, "", &ConfigPath::new(), discovery)?;
            Ok(merged)
        }
    }
}

/// Type name of a [`Value`] for error messages, in the same vocabulary as
/// [`LeafType::name`](crate::runtime::LeafType) (so "expected map, got
/// string" reads consistently).
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::PathSegment;
    use crate::runtime::Field as RtField;

    fn test_schema() -> Schema {
        Schema::object("App")
            .doc("App config")
            .field("port", RtField::integer().default(8080i64))
            .field("host", RtField::string().default("localhost"))
            .field("name", RtField::string()) // required, no default
            .field(
                "level",
                RtField::enum_of(["debug", "info", "warn"]).default("info"),
            )
            .nested(
                "db",
                Schema::object("Db")
                    .field("url", RtField::string().optional())
                    .field("pool_size", RtField::integer().default(5i64)),
            )
            .build()
    }

    fn parse(toml_text: &str) -> Map {
        crate::fixtures::test::parse_toml(toml_text)
    }

    /// Schema-only finalize: empty origins, no probe record. Production
    /// resolve injects the merged origin tree and discovery via
    /// [`super::finalize`].
    fn finalize(merged: Map, schema: &Schema) -> Result<Map, ClapfigError> {
        super::finalize(merged, &OriginMap::new(), schema, &DiscoveryRecord::empty())
    }

    fn fill(table: &mut Map, schema: &Schema) {
        fill_defaults_into(table, &mut OriginMap::new(), schema);
    }

    fn unknown_paths(table: &Map, schema: &Schema) -> Vec<String> {
        let mut unknown = Vec::new();
        collect_unknown_paths(table, schema, "", &ConfigPath::new(), &mut unknown);
        unknown.into_iter().map(|u| u.path).collect()
    }

    // --- collect_unknown_paths ---

    #[test]
    fn walker_accepts_known_keys() {
        let table = parse("port = 1\nname = \"x\"\n");
        assert!(unknown_paths(&table, &test_schema()).is_empty());
    }

    #[test]
    fn walker_flags_top_level_typo() {
        let table = parse("name = \"x\"\ntypo = 1\n");
        assert_eq!(unknown_paths(&table, &test_schema()), vec!["typo"]);
    }

    #[test]
    fn walker_flags_nested_typo() {
        let table = parse("name = \"x\"\n[db]\ntypo = 1\n");
        assert_eq!(unknown_paths(&table, &test_schema()), vec!["db.typo"]);
    }

    #[test]
    fn walker_recurses_into_array_of_entries() {
        // The unified walker recurses into `ArrayOf` indexed entries — the
        // behavior that used to exist only on the per-file walker. The env
        // layer shares this walker; env dotted keys can't build
        // arrays-of-tables, so the arm is simply inert there.
        let schema = Schema::object("App")
            .array_of(
                "plugins",
                Schema::object("Plugin").field("name", RtField::string().optional()),
            )
            .build();
        let table = parse("[[plugins]]\nname = \"a\"\n[[plugins]]\nrogue = 1\n");
        assert_eq!(unknown_paths(&table, &schema), vec!["plugins[1].rogue"]);
    }

    #[test]
    fn walker_recurses_into_map_of_entries() {
        let schema = Schema::object("App")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("name", RtField::string().optional()),
            )
            .build();
        let table = parse("[plugins.audit]\nrogue = 1\n");
        assert_eq!(unknown_paths(&table, &schema), vec!["plugins.audit.rogue"]);
    }

    #[test]
    fn walker_keeps_quoted_dotted_map_of_key_as_one_segment() {
        let schema = Schema::object("App")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("name", RtField::string().optional()),
            )
            .build();
        let table = parse("[plugins.\"acme.prod\"]\nrogue = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths(&table, &schema, "", &ConfigPath::new(), &mut unknown);
        assert_eq!(unknown.len(), 1);
        assert_eq!(
            unknown[0].config_path.segments(),
            [
                PathSegment::Key("plugins".into()),
                PathSegment::Key("acme.prod".into()),
                PathSegment::Key("rogue".into()),
            ]
        );
    }

    // --- fill_defaults_into ---

    #[test]
    fn fill_defaults_populates_missing_top_level() {
        let mut table = parse("name = \"x\"\n");
        fill(&mut table, &test_schema());
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
        assert_eq!(table.get("level"), Some(&Value::String("info".into())));
    }

    #[test]
    fn fill_defaults_does_not_overwrite() {
        let mut table = parse("name = \"x\"\nport = 9999\n");
        fill(&mut table, &test_schema());
        assert_eq!(table.get("port"), Some(&Value::Integer(9999)));
    }

    #[test]
    fn fill_defaults_creates_nested_section_when_missing() {
        let mut table = parse("name = \"x\"\n");
        fill(&mut table, &test_schema());
        let db = table.get("db").and_then(Value::as_map).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(5)));
        // `url` is optional; should stay absent.
        assert!(db.get("url").is_none());
    }

    #[test]
    fn fill_defaults_materializes_absent_array_of_as_empty_array() {
        // Absence means "no entries" — and the typed path's serde
        // deserialize needs the `[]` present to produce an empty `Vec`
        // instead of a missing-field error (same rule as `MapOf` → `{}`).
        let schema = Schema::object("App")
            .array_of(
                "plugins",
                Schema::object("Plugin").field("name", RtField::string().optional()),
            )
            .build();
        let mut table = parse("");
        fill(&mut table, &schema);
        assert_eq!(table.get("plugins"), Some(&Value::Array(Vec::new())));
    }

    #[test]
    fn fill_defaults_pushes_entry_defaults_into_existing_array_of_entries() {
        let schema = Schema::object("App")
            .array_of(
                "plugins",
                Schema::object("Plugin").field("priority", RtField::integer().default(10i64)),
            )
            .build();
        let mut table = parse("[[plugins]]\n");
        fill(&mut table, &schema);
        let items = table.get("plugins").and_then(Value::as_array).unwrap();
        let entry = items[0].as_map().unwrap();
        assert_eq!(entry.get("priority"), Some(&Value::Integer(10)));
    }

    #[test]
    fn fill_defaults_materializes_absent_array_leaf_as_empty_array() {
        // Non-optional array leaves without a declared default follow the
        // same absence rule as the structural `ArrayOf` (and as map
        // leaves follow `MapOf`'s): an absent array is the empty array.
        let schema = Schema::object("App")
            .field(
                "tags",
                RtField::array_of_type(crate::runtime::LeafType::String),
            )
            .build();
        let mut table = parse("");
        fill(&mut table, &schema);
        assert_eq!(table.get("tags"), Some(&Value::Array(Vec::new())));
    }

    #[test]
    fn fill_defaults_leaves_optional_array_leaf_absent() {
        // `Option<Vec<..>>` keeps the presence signal: no `[]` synthesis.
        let schema = Schema::object("App")
            .field(
                "tags",
                RtField::array_of_type(crate::runtime::LeafType::String).optional(),
            )
            .build();
        let mut table = parse("");
        fill(&mut table, &schema);
        assert!(!table.contains_key("tags"));
    }

    fn plugin_with_timeout() -> Schema {
        Schema::object("Plugin")
            .field("timeout", RtField::integer().default(30i64))
            .build()
    }

    #[test]
    fn fill_defaults_recurses_through_array_of_map_of_object() {
        let schema = Schema::object("App")
            .field(
                "groups",
                RtField::array_of_type(RtField::map_of(plugin_with_timeout())),
            )
            .build();
        let mut core = Map::new();
        core.insert("core".into(), Value::Map(Map::new()));
        let mut table = Map::new();
        table.insert("groups".into(), Value::Array(vec![Value::Map(core)]));
        fill(&mut table, &schema);
        let groups = table.get("groups").and_then(Value::as_array).unwrap();
        let core = groups[0]
            .as_map()
            .unwrap()
            .get("core")
            .unwrap()
            .as_map()
            .unwrap();
        assert_eq!(core.get("timeout"), Some(&Value::Integer(30)));
    }

    #[test]
    fn fill_defaults_recurses_through_map_of_array_of_object() {
        let schema = Schema::object("App")
            .field(
                "groups",
                RtField::map_of(RtField::array_of_type(plugin_with_timeout())),
            )
            .build();
        let mut groups = Map::new();
        groups.insert("core".into(), Value::Array(vec![Value::Map(Map::new())]));
        let mut table = Map::new();
        table.insert("groups".into(), Value::Map(groups));
        fill(&mut table, &schema);
        let core = table
            .get("groups")
            .and_then(Value::as_map)
            .unwrap()
            .get("core")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(
            core[0].as_map().unwrap().get("timeout"),
            Some(&Value::Integer(30))
        );
    }

    #[test]
    fn fill_defaults_walks_inserted_container_default_into_nested_objects() {
        let mut default_entry = Map::new();
        default_entry.insert("core".into(), Value::Map(Map::new()));
        let schema = Schema::object("App")
            .field(
                "groups",
                RtField::array_of_type(RtField::map_of(plugin_with_timeout()))
                    .default(Value::Array(vec![Value::Map(default_entry)])),
            )
            .build();
        let mut table = Map::new();
        fill(&mut table, &schema);
        let groups = table.get("groups").and_then(Value::as_array).unwrap();
        let core = groups[0]
            .as_map()
            .unwrap()
            .get("core")
            .unwrap()
            .as_map()
            .unwrap();
        assert_eq!(core.get("timeout"), Some(&Value::Integer(30)));
    }

    // --- finalize: required-field check ---

    #[test]
    fn finalize_errors_on_missing_required() {
        let schema = test_schema();
        let mut table = parse("port = 1\n");
        fill(&mut table, &schema);
        let err = finalize(table, &schema).unwrap_err();
        match err {
            ClapfigError::MissingRequired { key, .. } => assert_eq!(key, "name"),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn finalize_accepts_when_required_present() {
        let schema = test_schema();
        let mut table = parse("name = \"x\"\n");
        fill(&mut table, &schema);
        let out = finalize(table, &schema).unwrap();
        assert_eq!(out.get("name"), Some(&Value::String("x".into())));
        assert_eq!(out.get("port"), Some(&Value::Integer(8080)));
    }

    // --- finalize: type check ---

    #[test]
    fn finalize_rejects_wrong_leaf_type() {
        let schema = test_schema();
        let mut table = parse("name = \"x\"\nport = \"oops\"\n");
        fill(&mut table, &schema);
        let err = finalize(table, &schema).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "port");
                assert!(reason.contains("expected integer"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn finalize_rejects_out_of_set_enum_value() {
        let schema = test_schema();
        let mut table = parse("name = \"x\"\nlevel = \"garbage\"\n");
        fill(&mut table, &schema);
        let err = finalize(table, &schema).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "level");
                assert!(reason.contains("not in allowed set"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    // --- finalize: schema-driven datetime coercion (ADR-0001) ---

    fn datetime_schema() -> Schema {
        Schema::object("T")
            .field("dt", RtField::datetime().optional())
            .field(
                "dts",
                RtField::array_of_type(crate::runtime::LeafType::DateTime).optional(),
            )
            .build()
    }

    #[test]
    fn finalize_coerces_datetime_strings_in_all_four_lexical_forms() {
        // TOML's four datetime forms: offset date-time, local date-time,
        // local date, local time. Schema-blind sources deliver them as
        // strings; the DateTime leaf declaration coerces each.
        let schema = datetime_schema();
        for form in [
            "1979-05-27T07:32:00Z",
            "1979-05-27T07:32:00",
            "1979-05-27",
            "07:32:00",
        ] {
            let mut table = Map::new();
            table.insert("dt".into(), Value::String(form.into()));
            let out = finalize(table, &schema).unwrap();
            match &out["dt"] {
                Value::Datetime(dt) => assert_eq!(dt.to_string(), form),
                other => panic!("{form}: expected Datetime, got {other:?}"),
            }
        }
    }

    #[test]
    fn finalize_coerces_datetimes_inside_declared_arrays() {
        let schema = datetime_schema();
        let mut table = Map::new();
        table.insert(
            "dts".into(),
            Value::Array(vec![Value::String("1979-05-27".into())]),
        );
        let out = finalize(table, &schema).unwrap();
        match &out["dts"][0] {
            Value::Datetime(dt) => assert_eq!(dt.to_string(), "1979-05-27"),
            other => panic!("expected Datetime, got {other:?}"),
        }
    }

    #[test]
    fn finalize_rejects_malformed_datetime_string_as_type_error() {
        // A string matching none of the four forms is a normal type
        // error, never a silent pass-through.
        let schema = datetime_schema();
        for bad in ["not-a-date", "1979-13-45", "07:99:00", "2024/01/02"] {
            let mut table = Map::new();
            table.insert("dt".into(), Value::String(bad.into()));
            match finalize(table, &schema) {
                Err(ClapfigError::InvalidValue { key, reason, .. }) => {
                    assert_eq!(key, "dt");
                    assert!(reason.contains("expected datetime"), "{bad}: {reason}");
                }
                other => panic!("{bad}: expected InvalidValue, got {other:?}"),
            }
        }
    }

    #[test]
    fn finalize_never_coerces_strings_on_non_datetime_leaves() {
        // Coercion is schema-driven, not sniffing: a datetime-looking
        // string on a String leaf stays a string.
        let schema = Schema::object("T")
            .field("s", RtField::string().optional())
            .build();
        let mut table = Map::new();
        table.insert("s".into(), Value::String("1979-05-27".into()));
        let out = finalize(table, &schema).unwrap();
        assert_eq!(out["s"], Value::String("1979-05-27".into()));
    }

    // --- finalize: integer-for-float coercion ---

    #[test]
    fn finalize_coerces_integer_to_float_on_float_leaves() {
        // serde accepts `timeout = 5` for an f64 field; finalize rewrites
        // the merged value so Map-out consumers see a float too.
        let schema = Schema::object("T")
            .field("timeout", RtField::float().optional())
            .field(
                "rates",
                RtField::array_of_type(crate::runtime::LeafType::Float).optional(),
            )
            .build();
        let mut table = Map::new();
        table.insert("timeout".into(), Value::Integer(5));
        table.insert(
            "rates".into(),
            Value::Array(vec![Value::Integer(1), Value::Float(2.5)]),
        );
        let out = finalize(table, &schema).unwrap();
        assert_eq!(out["timeout"], Value::Float(5.0));
        assert_eq!(
            out["rates"],
            Value::Array(vec![Value::Float(1.0), Value::Float(2.5)])
        );
    }

    #[test]
    fn finalize_never_coerces_integers_on_integer_leaves() {
        let schema = Schema::object("T")
            .field("count", RtField::integer().optional())
            .build();
        let mut table = Map::new();
        table.insert("count".into(), Value::Integer(5));
        let out = finalize(table, &schema).unwrap();
        assert_eq!(out["count"], Value::Integer(5));
    }

    // --- finalize: integer bounds ---

    #[test]
    fn finalize_reports_out_of_range_integer_with_key_path() {
        // A `u8`-shaped leaf rejects 300 at the schema check, naming the
        // key — not as a `<merged>` deserialize failure downstream.
        let schema = Schema::object("T")
            .nested(
                "server",
                Schema::object("Server").field(
                    "retries",
                    RtField::integer_in(Some(0), Some(255)).optional(),
                ),
            )
            .build();
        let table = parse("[server]\nretries = 300\n");
        let err = finalize(table, &schema).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "server.retries");
                assert!(reason.contains("out of range"), "{reason}");
                assert!(reason.contains("0..=255"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn finalize_nested_required_check() {
        // Supply every top-level required field by hand so the only missing
        // required field is `db.pool_size` (which has a default but is
        // required if no layer provides it). Skipping `fill_defaults_into`
        // exposes the nested-required path.
        let schema = test_schema();
        let table = parse("name = \"x\"\nport = 8080\nhost = \"h\"\nlevel = \"info\"\n[db]\n");
        let err = finalize(table, &schema).unwrap_err();
        match err {
            ClapfigError::MissingRequired { key, .. } => assert_eq!(key, "db.pool_size"),
            other => panic!("expected MissingRequired(db.pool_size), got {other:?}"),
        }
    }

    #[test]
    fn nested_missing_leaf_carries_injected_discovery_not_an_origin() {
        // Parent map present (`[db]`), required leaf absent. Same
        // MissingRequired diagnostic as a top-level miss — no nearest-
        // ancestor origin. The probe record is the one the caller injected.
        let schema = test_schema();
        let table = parse("name = \"x\"\nport = 8080\nhost = \"h\"\nlevel = \"info\"\n[db]\n");
        let discovery = DiscoveryRecord {
            files: vec![crate::error::FileProbe {
                path: "/tmp/app.toml".into(),
                outcome: crate::error::ProbeOutcome::Loaded,
            }],
            env: true,
            url: false,
            overrides: false,
        };
        let err = super::finalize(table, &OriginMap::new(), &schema, &discovery).unwrap_err();
        match err {
            ClapfigError::MissingRequired {
                key,
                discovery: got,
            } => {
                assert_eq!(key, "db.pool_size");
                assert_eq!(got, discovery);
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
        let msg = ClapfigError::missing_required("db.pool_size", discovery).to_string();
        assert!(
            !msg.contains("set by"),
            "MissingRequired must not name a winning origin: {msg}"
        );
    }

    #[test]
    fn fill_defaults_writes_default_origins_in_the_same_walk() {
        let mut table = parse("name = \"x\"\n");
        let mut origins = OriginMap::new();
        fill_defaults_into(&mut table, &mut origins, &test_schema());
        let port = crate::origin::lookup(&origins, &ConfigPath::new().key("port")).unwrap();
        assert_eq!(port.layer, crate::types::InputType::Default);
        assert_eq!(port.key.as_deref(), Some("port"));
        let pool =
            crate::origin::lookup(&origins, &ConfigPath::new().key("db").key("pool_size")).unwrap();
        assert_eq!(pool.layer, crate::types::InputType::Default);
        assert_eq!(pool.key.as_deref(), Some("db.pool_size"));
    }

    #[test]
    fn fill_defaults_does_not_overwrite_existing_origins() {
        let mut table = parse("name = \"x\"\nport = 9999\n");
        let mut origins = OriginMap::new();
        origins.insert("port".into(), OriginNode::leaf(Origin::r#override("port")));
        fill_defaults_into(&mut table, &mut origins, &test_schema());
        let port = crate::origin::lookup(&origins, &ConfigPath::new().key("port")).unwrap();
        assert_eq!(port.layer, crate::types::InputType::Override);
        assert_eq!(table.get("port"), Some(&Value::Integer(9999)));
    }

    #[test]
    fn coerce_does_not_change_origin() {
        let schema = Schema::object("T")
            .field("timeout", RtField::float().optional())
            .build();
        let mut table = Map::new();
        table.insert("timeout".into(), Value::Integer(5));
        let mut origins = OriginMap::new();
        origins.insert(
            "timeout".into(),
            OriginNode::leaf(Origin::env(vec!["APP__TIMEOUT".into()])),
        );
        let before = origins.clone();
        coerce_leaf_values(&mut table, &schema);
        assert_eq!(table["timeout"], Value::Float(5.0));
        assert_eq!(origins, before);
        assert_eq!(
            crate::origin::lookup(&origins, &ConfigPath::new().key("timeout"))
                .unwrap()
                .layer,
            crate::types::InputType::Env
        );
    }

    fn tagged_block() -> crate::runtime::TaggedShape {
        crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("mount", RtField::string())
                    .field("crate_path", RtField::string().optional())
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field("mount", RtField::string())
                    .field("artifact", RtField::string())
                    .build(),
            )
            .variant("off", Schema::object("Off").build())
            .build()
    }

    fn unknown_paths_tagged(table: &Map, tagged: &crate::runtime::TaggedShape) -> Vec<String> {
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            table,
            DocumentRoot::Tagged(tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        unknown.into_iter().map(|u| u.path).collect()
    }

    fn exclusive_paths_tagged(table: &Map, tagged: &crate::runtime::TaggedShape) -> Vec<String> {
        let mut unknown = Vec::new();
        collect_branch_exclusive_root(table, DocumentRoot::Tagged(tagged), &mut unknown);
        unknown.into_iter().map(|u| u.path).collect()
    }

    #[test]
    fn tagged_phase1_accepts_tag_and_any_variant_field() {
        let tagged = tagged_block();
        let table = parse("kind = \"rust\"\nmount = \".\"\nartifact = \"x\"\n");
        assert!(unknown_paths_tagged(&table, &tagged).is_empty());
    }

    #[test]
    fn tagged_phase1_flags_true_unknown_without_requiring_discriminator() {
        let tagged = tagged_block();
        let table = parse("mount = \".\"\nnot_a_field = 1\n");
        assert_eq!(unknown_paths_tagged(&table, &tagged), vec!["not_a_field"]);
    }

    #[test]
    fn tagged_phase1_does_not_flag_branch_exclusive_keys() {
        let tagged = tagged_block();
        let table = parse("kind = \"rust\"\nmount = \".\"\nartifact = \"x\"\n");
        assert!(unknown_paths_tagged(&table, &tagged).is_empty());
        assert_eq!(exclusive_paths_tagged(&table, &tagged), vec!["artifact"]);
    }

    #[test]
    fn tagged_phase2_skips_true_unknowns() {
        let tagged = tagged_block();
        let table = parse("kind = \"rust\"\nmount = \".\"\nnot_a_field = 1\n");
        assert_eq!(unknown_paths_tagged(&table, &tagged), vec!["not_a_field"]);
        assert!(exclusive_paths_tagged(&table, &tagged).is_empty());
    }

    #[test]
    fn tagged_finalize_good_rust_instance() {
        let tagged = tagged_block();
        let table = parse("kind = \"rust\"\nmount = \".\"\n");
        let out = finalize_root(
            table,
            &OriginMap::new(),
            DocumentRoot::Tagged(&tagged),
            &DiscoveryRecord::empty(),
        )
        .unwrap();
        assert_eq!(out["kind"], Value::String("rust".into()));
        assert_eq!(out["mount"], Value::String(".".into()));
    }

    #[test]
    fn tagged_finalize_unit_variant_is_tag_only() {
        let tagged = tagged_block();
        let table = parse("kind = \"off\"\n");
        let out = finalize_root(
            table,
            &OriginMap::new(),
            DocumentRoot::Tagged(&tagged),
            &DiscoveryRecord::empty(),
        )
        .unwrap();
        assert_eq!(out["kind"], Value::String("off".into()));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn tagged_finalize_unknown_discriminator_is_invalid_value_with_allowed_set() {
        let tagged = tagged_block();
        let table = parse("kind = \"rus\"\nmount = \".\"\n");
        let err = finalize_root(
            table,
            &OriginMap::new(),
            DocumentRoot::Tagged(&tagged),
            &DiscoveryRecord::empty(),
        )
        .unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "kind");
                assert!(reason.contains("not in allowed set"), "{reason}");
                assert!(reason.contains("rust"), "{reason}");
                assert!(reason.contains("payload"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn tagged_finalize_missing_tag_is_missing_required_with_discovery() {
        let tagged = tagged_block();
        let table = parse("mount = \".\"\n");
        let discovery = DiscoveryRecord {
            files: vec![crate::error::FileProbe {
                path: "app.toml".into(),
                outcome: crate::error::ProbeOutcome::Loaded,
            }],
            env: true,
            ..DiscoveryRecord::empty()
        };
        let err = finalize_root(
            table,
            &OriginMap::new(),
            DocumentRoot::Tagged(&tagged),
            &discovery,
        )
        .unwrap_err();
        match err {
            ClapfigError::MissingRequired { key, discovery: d } => {
                assert_eq!(key, "kind");
                assert_eq!(d.files.len(), 1);
                assert!(d.env);
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
        let msg = ClapfigError::missing_required("kind", discovery).to_string();
        assert!(
            !msg.contains("set by"),
            "MissingRequired must not name a winning origin: {msg}"
        );
    }

    #[test]
    fn tagged_fill_defaults_only_selected_variant() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("mount", RtField::string().default("."))
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field("artifact", RtField::string().default("none"))
                    .build(),
            )
            .build();
        let mut table = parse("kind = \"rust\"\n");
        let mut origins = OriginMap::new();
        fill_defaults_into_root(&mut table, &mut origins, DocumentRoot::Tagged(&tagged));
        assert_eq!(table.get("mount"), Some(&Value::String(".".into())));
        assert!(!table.contains_key("artifact"));
    }

    fn mixed_params_tagged() -> crate::runtime::TaggedShape {
        crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "typed",
                Schema::object("Typed")
                    .nested(
                        "params",
                        Schema::object("P").field("shape", RtField::string().optional()),
                    )
                    .build(),
            )
            .variant(
                "open",
                Schema::object("Open")
                    .field("params", RtField::value().optional())
                    .build(),
            )
            .build()
    }

    #[test]
    fn phase1_union_keeps_keys_valid_for_a_value_alternative() {
        let tagged = mixed_params_tagged();
        let table = parse("kind = \"open\"\n[params]\nanything = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            &table,
            DocumentRoot::Tagged(&tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        assert!(
            unknown.is_empty(),
            "params.anything is known via the Value alternative: {:?}",
            unknown.iter().map(|u| u.path.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn phase1_union_still_flags_true_unknowns_outside_every_alternative() {
        let tagged = mixed_params_tagged();
        let table = parse("kind = \"open\"\nrogue = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            &table,
            DocumentRoot::Tagged(&tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        assert_eq!(
            unknown.iter().map(|u| u.path.as_str()).collect::<Vec<_>>(),
            ["rogue"]
        );
    }

    #[test]
    fn phase2_reports_exclusive_keys_from_a_tagged_alternative_of_an_object_field() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .nested(
                        "params",
                        Schema::object("P").field("shape", RtField::string().optional()),
                    )
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field(
                        "params",
                        crate::runtime::Shape::from(
                            crate::runtime::Shape::tagged("Params", "kind")
                                .variant(
                                    "artifact",
                                    Schema::object("A")
                                        .field("entry", RtField::string().optional())
                                        .build(),
                                )
                                .build(),
                        ),
                    )
                    .build(),
            )
            .build();
        let table = parse("kind = \"rust\"\n[params]\nkind = \"artifact\"\nentry = \"x\"\n");
        let mut exclusive = Vec::new();
        collect_branch_exclusive_root(&table, DocumentRoot::Tagged(&tagged), &mut exclusive);
        let paths: Vec<&str> = exclusive.iter().map(|u| u.path.as_str()).collect();
        assert!(
            paths.contains(&"params.kind") || paths.contains(&"params.entry"),
            "branch-exclusive keys from the other constructor must be reported: {paths:?}"
        );
    }

    #[test]
    fn phase1_value_leaf_does_not_recurse_into_structured_alternative() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "typed",
                Schema::object("Typed").nested(
                    "params",
                    Schema::object("P").nested(
                        "inner",
                        Schema::object("I").field("shape", RtField::string().optional()),
                    ),
                ),
            )
            .variant(
                "open",
                Schema::object("Open")
                    .field("params", RtField::value().optional())
                    .build(),
            )
            .build();
        let table = parse("kind = \"open\"\n[params.inner]\nfoo = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            &table,
            DocumentRoot::Tagged(&tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        assert!(
            unknown.is_empty(),
            "Value alternative accepts the whole params map: {:?}",
            unknown.iter().map(|u| u.path.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn phase1_value_leaf_does_not_recurse_into_array_alternative() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "typed",
                Schema::object("Typed").field(
                    "items",
                    crate::runtime::Shape::array(
                        "items",
                        Schema::object("Item").field("name", RtField::string().optional()),
                    ),
                ),
            )
            .variant(
                "open",
                Schema::object("Open")
                    .field("items", RtField::value().optional())
                    .build(),
            )
            .build();
        let table = parse("kind = \"open\"\n[[items]]\nname = \"x\"\nrogue = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            &table,
            DocumentRoot::Tagged(&tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        assert!(
            unknown.is_empty(),
            "Value alternative accepts the whole items array: {:?}",
            unknown.iter().map(|u| u.path.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn phase1_value_leaf_does_not_recurse_into_map_alternative() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "typed",
                Schema::object("Typed").field(
                    "params",
                    crate::runtime::Shape::map(
                        "params",
                        Schema::object("P").field("name", RtField::string().optional()),
                    ),
                ),
            )
            .variant(
                "open",
                Schema::object("Open")
                    .field("params", RtField::value().optional())
                    .build(),
            )
            .build();
        let table = parse("kind = \"open\"\n[params.core]\nrogue = 1\n");
        let mut unknown = Vec::new();
        collect_unknown_paths_root(
            &table,
            DocumentRoot::Tagged(&tagged),
            "",
            &ConfigPath::new(),
            &mut unknown,
        );
        assert!(
            unknown.is_empty(),
            "Value alternative accepts the whole params map: {:?}",
            unknown.iter().map(|u| u.path.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn phase2_recurses_tagged_selected_fields_against_other_constructors() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "tagged_params",
                Schema::object("T").field(
                    "params",
                    crate::runtime::Shape::from(
                        crate::runtime::Shape::tagged("Params", "t")
                            .variant(
                                "x",
                                Schema::object("X").nested(
                                    "meta",
                                    Schema::object("XM")
                                        .field("crate_path", RtField::string().optional()),
                                ),
                            )
                            .variant(
                                "y",
                                Schema::object("Y").nested(
                                    "meta",
                                    Schema::object("YM")
                                        .field("artifact", RtField::string().optional()),
                                ),
                            )
                            .build(),
                    ),
                ),
            )
            .variant(
                "object_params",
                Schema::object("O").nested(
                    "params",
                    Schema::object("P").nested(
                        "meta",
                        Schema::object("PM").field("artifact", RtField::string().optional()),
                    ),
                ),
            )
            .build();
        let table = parse(
            "kind = \"tagged_params\"\n[params]\nt = \"x\"\n[params.meta]\nartifact = \"z\"\n",
        );
        let mut exclusive = Vec::new();
        collect_branch_exclusive_root(&table, DocumentRoot::Tagged(&tagged), &mut exclusive);
        let paths: Vec<&str> = exclusive.iter().map(|u| u.path.as_str()).collect();
        assert_eq!(
            paths
                .iter()
                .filter(|p| **p == "params.meta.artifact")
                .count(),
            1,
            "intra-shape and inter-shape exclusive keys must be reported once: {paths:?}"
        );
    }

    fn array_of_tagged_schema() -> Schema {
        Schema::object("App")
            .field(
                "blocks",
                Shape::array("blocks", Shape::from(tagged_block())),
            )
            .build()
    }

    fn map_of_array_of_tagged_schema() -> Schema {
        Schema::object("App")
            .field(
                "groups",
                RtField::map_of(Shape::array("blocks", Shape::from(tagged_block()))),
            )
            .build()
    }

    fn exclusive_paths(table: &Map, schema: &Schema) -> Vec<String> {
        let mut unknown = Vec::new();
        collect_branch_exclusive_root(table, DocumentRoot::Object(schema), &mut unknown);
        unknown.into_iter().map(|u| u.path).collect()
    }

    #[test]
    fn array_of_tagged_phase1_flags_true_unknown_and_skips_branch_exclusive() {
        let schema = array_of_tagged_schema();
        let table = parse(
            "[[blocks]]\nkind = \"rust\"\nmount = \".\"\nartifact = \"x\"\nnot_a_field = 1\n",
        );
        assert_eq!(
            unknown_paths(&table, &schema),
            vec!["blocks[0].not_a_field"]
        );
        assert_eq!(exclusive_paths(&table, &schema), vec!["blocks[0].artifact"]);
    }

    #[test]
    fn map_of_array_of_tagged_phase1_and_phase2_index_the_entry() {
        let schema = map_of_array_of_tagged_schema();
        let table = parse(
            "[[groups.core]]\nkind = \"rust\"\nmount = \".\"\nartifact = \"x\"\nnot_a_field = 1\n",
        );
        assert_eq!(
            unknown_paths(&table, &schema),
            vec!["groups.core[0].not_a_field"]
        );
        assert_eq!(
            exclusive_paths(&table, &schema),
            vec!["groups.core[0].artifact"]
        );
    }

    #[test]
    fn array_of_tagged_fill_defaults_only_selected_variant() {
        let tagged = crate::runtime::Shape::tagged("Block", "kind")
            .variant(
                "rust",
                Schema::object("Rust")
                    .field("mount", RtField::string().default("."))
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .field("artifact", RtField::string().default("none"))
                    .build(),
            )
            .build();
        let schema = Schema::object("App")
            .field("blocks", Shape::array("blocks", Shape::from(tagged)))
            .build();
        let mut table = parse("[[blocks]]\nkind = \"rust\"\n");
        fill(&mut table, &schema);
        let item = table["blocks"].as_array().unwrap()[0].as_map().unwrap();
        assert_eq!(item.get("mount"), Some(&Value::String(".".into())));
        assert!(!item.contains_key("artifact"));
    }

    #[test]
    fn array_of_tagged_finalize_unknown_discriminator_and_missing_tag() {
        let schema = array_of_tagged_schema();
        let err = finalize(
            parse("[[blocks]]\nkind = \"rus\"\nmount = \".\"\n"),
            &schema,
        )
        .unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "blocks[0].kind");
                assert!(reason.contains("not in allowed set"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
        let err = finalize(parse("[[blocks]]\nmount = \".\"\n"), &schema).unwrap_err();
        match err {
            ClapfigError::MissingRequired { key, .. } => {
                assert_eq!(key, "blocks[0].kind");
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn array_of_tagged_finalize_good_items() {
        let schema = array_of_tagged_schema();
        let out = finalize(
            parse("[[blocks]]\nkind = \"rust\"\nmount = \".\"\n[[blocks]]\nkind = \"off\"\n"),
            &schema,
        )
        .unwrap();
        let items = out["blocks"].as_array().unwrap();
        assert_eq!(
            items[0].as_map().unwrap()["kind"],
            Value::String("rust".into())
        );
        assert_eq!(
            items[1].as_map().unwrap()["kind"],
            Value::String("off".into())
        );
    }
}
