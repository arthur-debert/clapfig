//! Schema-driven walks of a value [`Map`] against a [`Schema`]: the
//! resolve pipeline's validation stages as free functions.
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
//! [`UnknownKey`]: crate::validate::UnknownKey

use crate::error::{ClapfigError, DiscoveryRecord};
use crate::format::ConfigPath;
use crate::origin::{Origin, OriginMap, OriginNode};
use crate::runtime::{Field, NamedField, Schema};
use crate::validate::UnknownKey;
use crate::value::{Map, Value};

/// Recursively walk `table` against `schema`, collecting unknown keys.
///
/// `prefix` is the dotted display form (cascade / error rendering).
/// `path` is the structured address of `table` — the same [`ConfigPath`]
/// the adapter indexed — so span lookup does not reconstruct from the
/// display string (a quoted dotted MapOf key stays one segment).
///
/// For nested objects (`Field::Nested`) the recursion descends into the
/// sub-table; for `Field::ArrayOf`, each entry is validated against the
/// item schema (with an `[index]` path segment); for `Field::MapOf`,
/// each entry's value is validated against the item schema (with the
/// user-supplied key forming a path segment).
///
/// The same walker serves the per-file pass and the env layer. Env
/// dotted-key syntax cannot express arrays-of-tables, so the `ArrayOf`
/// arm simply never fires there — recursion support is harmless.
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
            Some(NamedField {
                field: Field::Leaf(_),
                ..
            }) => {
                // Leaf — type checking happens later in `finalize`.
            }
            Some(NamedField {
                field: Field::Nested(nested),
                ..
            }) => {
                if let Value::Map(t) = value {
                    collect_unknown_paths(t, nested, &full, &child, unknown);
                }
            }
            Some(NamedField {
                field: Field::ArrayOf(item_schema),
                ..
            }) => {
                if let Value::Array(items) = value {
                    for (i, item) in items.iter().enumerate() {
                        if let Value::Map(t) = item {
                            let indexed = format!("{full}[{i}]");
                            collect_unknown_paths(
                                t,
                                item_schema,
                                &indexed,
                                &child.clone().index(i),
                                unknown,
                            );
                        }
                    }
                }
            }
            Some(NamedField {
                field: Field::MapOf(item_schema),
                ..
            }) => {
                // Each map entry's value is a nested object. The entry key
                // forms a path segment, so `plugins.audit.rogue` is the
                // path for an unknown key inside the `audit` entry of a
                // `plugins` MapOf. A quoted dotted entry key is one
                // [`ConfigPath`] segment even though `prefix` flattens it.
                if let Value::Map(entries) = value {
                    for (entry_key, entry_value) in entries {
                        if let Value::Map(t) = entry_value {
                            let entry_path = format!("{full}.{entry_key}");
                            collect_unknown_paths(
                                t,
                                item_schema,
                                &entry_path,
                                &child.clone().key(entry_key),
                                unknown,
                            );
                        }
                    }
                }
            }
        }
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
        match &nf.field {
            Field::Leaf(leaf) => {
                if !table.contains_key(&nf.name) {
                    if let Some(default) = &leaf.default {
                        insert_default(
                            table,
                            origins,
                            &nf.name,
                            default.clone(),
                            child.as_ref(),
                            &schema_key,
                            filled,
                        );
                    } else if !leaf.optional && matches!(leaf.ty, crate::runtime::LeafType::Map(_))
                    {
                        // A non-optional map leaf (bare `HashMap<String,
                        // scalar>`, or `HashMap<String, UnitEnum>` flattened
                        // to `Map(Enum)`) follows the `MapOf` absence rule:
                        // entries are user-supplied, so an absent map is the
                        // empty map — materialized here so the required
                        // check passes and the typed deserialize yields an
                        // empty map instead of a missing-field error.
                        // Optional (`Option<Map<..>>`) leaves stay absent
                        // and deserialize to `None`.
                        insert_default(
                            table,
                            origins,
                            &nf.name,
                            Value::Map(Map::new()),
                            child.as_ref(),
                            &schema_key,
                            filled,
                        );
                    } else if !leaf.optional
                        && matches!(leaf.ty, crate::runtime::LeafType::Array(_))
                    {
                        // A non-optional array leaf without an explicit
                        // default (bare `Vec<scalar>`, or `Vec<UnitEnum>`
                        // flattened to `Array(Enum)`) follows the `ArrayOf`
                        // absence rule the same way map leaves follow
                        // `MapOf`'s: entries are user-supplied, so an absent
                        // array is the empty array. Optional
                        // (`Option<Vec<..>>`) leaves stay absent and
                        // deserialize to `None`; a declared
                        // `#[clapfig(default = [...])]` wins (branch above).
                        insert_default(
                            table,
                            origins,
                            &nf.name,
                            Value::Array(Vec::new()),
                            child.as_ref(),
                            &schema_key,
                            filled,
                        );
                    }
                }
            }
            Field::Nested(nested) => {
                let created = !table.contains_key(&nf.name);
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Map(Map::new()));
                if created {
                    origins.entry(nf.name.clone()).or_insert_with(|| {
                        OriginNode::map(Origin::default(&schema_key), OriginMap::new())
                    });
                    trace_default_filled(child.as_ref(), "map", filled);
                }
                if let Value::Map(t) = entry {
                    let child_origins = child_map_origins(origins, &nf.name, &schema_key);
                    fill_defaults_at(t, child_origins, nested, &schema_key, child, filled);
                }
            }
            Field::ArrayOf(item_schema) => {
                // Array entries are user-supplied — push defaults into
                // existing entries, never synthesize missing array items.
                // An absent array-of node itself materializes as the empty
                // array (same as `MapOf` below): absence means "no
                // entries", and the typed path's serde deserialize needs
                // the `[]` to produce an empty `Vec` instead of a
                // missing-field error.
                let created = !table.contains_key(&nf.name);
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if created {
                    origins.entry(nf.name.clone()).or_insert_with(|| {
                        OriginNode::array(Origin::default(&schema_key), Vec::new())
                    });
                    trace_default_filled(child.as_ref(), "array", filled);
                }
                if let Value::Array(items) = entry {
                    let child_origins = child_array_origins(origins, &nf.name, &schema_key);
                    for (i, item) in items.iter_mut().enumerate() {
                        if let Value::Map(t) = item {
                            let indexed = format!("{schema_key}[{i}]");
                            while child_origins.len() <= i {
                                child_origins.push(OriginNode::map(
                                    Origin::default(&indexed),
                                    OriginMap::new(),
                                ));
                            }
                            let entry_origins = child_origins[i].map_children_mut();
                            let indexed_path = child.as_ref().map(|p| p.clone().index(i));
                            fill_defaults_at(
                                t,
                                entry_origins,
                                item_schema,
                                &indexed,
                                indexed_path,
                                filled,
                            );
                        }
                    }
                }
            }
            Field::MapOf(item_schema) => {
                // Map entries are user-supplied — push defaults into
                // existing entries, never synthesize missing entries. An
                // absent map-of node itself materializes as the empty map
                // (same as `Nested` above): absence means "no entries",
                // and the typed path's serde deserialize needs the `{}` to
                // produce an empty `HashMap` instead of a missing-field
                // error.
                let created = !table.contains_key(&nf.name);
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Map(Map::new()));
                if created {
                    origins.entry(nf.name.clone()).or_insert_with(|| {
                        OriginNode::map(Origin::default(&schema_key), OriginMap::new())
                    });
                    trace_default_filled(child.as_ref(), "map", filled);
                }
                if let Value::Map(entries) = entry {
                    let child_origins = child_map_origins(origins, &nf.name, &schema_key);
                    for (entry_key, entry_value) in entries.iter_mut() {
                        if let Value::Map(t) = entry_value {
                            let entry_path = format!("{schema_key}.{entry_key}");
                            let entry_origins =
                                child_map_entry_origins(child_origins, entry_key, &entry_path);
                            let entry_cfg = child.as_ref().map(|p| p.clone().key(entry_key));
                            fill_defaults_at(
                                t,
                                entry_origins,
                                item_schema,
                                &entry_path,
                                entry_cfg,
                                filled,
                            );
                        }
                    }
                }
            }
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

fn child_array_origins<'a>(
    origins: &'a mut OriginMap,
    name: &str,
    schema_key: &str,
) -> &'a mut Vec<OriginNode> {
    let node = origins
        .entry(name.to_string())
        .or_insert_with(|| OriginNode::array(Origin::default(schema_key), Vec::new()));
    node.array_children_mut()
}

fn child_map_entry_origins<'a>(
    origins: &'a mut OriginMap,
    entry_key: &str,
    schema_key: &str,
) -> &'a mut OriginMap {
    let node = origins
        .entry(entry_key.to_string())
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
        match &nf.field {
            Field::Leaf(leaf) => {
                if let Some(value) = table.get_mut(&nf.name) {
                    coerce_value(value, &leaf.ty);
                }
            }
            Field::Nested(nested) => {
                if let Some(Value::Map(t)) = table.get_mut(&nf.name) {
                    coerce_leaf_values(t, nested);
                }
            }
            Field::ArrayOf(item_schema) => {
                if let Some(Value::Array(items)) = table.get_mut(&nf.name) {
                    for item in items {
                        if let Value::Map(t) = item {
                            coerce_leaf_values(t, item_schema);
                        }
                    }
                }
            }
            Field::MapOf(item_schema) => {
                if let Some(Value::Map(entries)) = table.get_mut(&nf.name) {
                    for entry_value in entries.values_mut() {
                        if let Value::Map(t) = entry_value {
                            coerce_leaf_values(t, item_schema);
                        }
                    }
                }
            }
        }
    }
}

/// Coerce one value against its declared leaf type (datetime strings on
/// datetime leaves, integers on float leaves), recursing through declared
/// containers (`Array` elements, homogeneous `Map` values). Shared with
/// the persist path, which validates `config set` values against the same
/// leaf declarations.
pub(crate) fn coerce_value(value: &mut Value, ty: &crate::runtime::LeafType) {
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
        LeafType::Array(elem) => {
            if let Value::Array(items) = value {
                for item in items {
                    coerce_value(item, elem);
                }
            }
        }
        LeafType::Map(elem) => {
            if let Value::Map(entries) = value {
                for entry in entries.values_mut() {
                    coerce_value(entry, elem);
                }
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
        match &nf.field {
            Field::Leaf(leaf) => match table.get(&nf.name) {
                None => {
                    if !leaf.optional {
                        return Err(ClapfigError::missing_required(display, discovery.clone()));
                    }
                }
                Some(value) => {
                    leaf.ty.check(value).map_err(|reason| {
                        ClapfigError::invalid_value_at(display.clone(), reason, origins, &child)
                    })?;
                }
            },
            Field::Nested(nested) => match table.get(&nf.name) {
                None => {
                    // A nested section is required if any of its leaves is
                    // required. Recurse with an empty table so the missing-
                    // required check below fires for inner leaves.
                    let empty = Map::new();
                    check_required_and_types(
                        &empty,
                        &OriginMap::new(),
                        nested,
                        &display,
                        &child,
                        discovery,
                    )?;
                }
                Some(Value::Map(inner)) => {
                    check_required_and_types(inner, origins, nested, &display, &child, discovery)?;
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value_at(
                        display,
                        format!("expected map, got {}", value_type_name(other)),
                        origins,
                        &child,
                    ));
                }
            },
            Field::ArrayOf(item_schema) => match table.get(&nf.name) {
                None => {
                    // Absent array-of: empty list is the natural default,
                    // not an error — `Vec<Nested>`-style fields can be
                    // legitimately empty.
                }
                Some(Value::Array(items)) => {
                    for (i, item) in items.iter().enumerate() {
                        let indexed = format!("{display}[{i}]");
                        let indexed_path = child.clone().index(i);
                        match item {
                            Value::Map(inner) => {
                                check_required_and_types(
                                    inner,
                                    origins,
                                    item_schema,
                                    &indexed,
                                    &indexed_path,
                                    discovery,
                                )?;
                            }
                            other => {
                                return Err(ClapfigError::invalid_value_at(
                                    indexed,
                                    format!("expected map, got {}", value_type_name(other)),
                                    origins,
                                    &indexed_path,
                                ));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value_at(
                        display,
                        format!("expected array, got {}", value_type_name(other)),
                        origins,
                        &child,
                    ));
                }
            },
            Field::MapOf(item_schema) => match table.get(&nf.name) {
                None => {
                    // Absent map-of: empty map is the natural default, not
                    // an error. Same rule as ArrayOf — user-supplied
                    // entries can be zero.
                }
                Some(Value::Map(entries)) => {
                    for (entry_key, entry_value) in entries {
                        let entry_path = format!("{display}.{entry_key}");
                        let entry_cfg = child.clone().key(entry_key);
                        match entry_value {
                            Value::Map(inner) => {
                                check_required_and_types(
                                    inner,
                                    origins,
                                    item_schema,
                                    &entry_path,
                                    &entry_cfg,
                                    discovery,
                                )?;
                            }
                            other => {
                                return Err(ClapfigError::invalid_value_at(
                                    entry_path,
                                    format!("expected map, got {}", value_type_name(other)),
                                    origins,
                                    &entry_cfg,
                                ));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value_at(
                        display,
                        format!("expected map, got {}", value_type_name(other)),
                        origins,
                        &child,
                    ));
                }
            },
        }
    }
    Ok(())
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
}
