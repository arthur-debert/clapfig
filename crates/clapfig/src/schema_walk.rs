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
//!   empty arrays (an absent array is the empty array).
//! - **[`finalize`]**: applies schema-driven coercions (string values in
//!   TOML's four datetime lexical forms become [`Value::Datetime`] on
//!   datetime leaves per ADR-0001; integer values become [`Value::Float`]
//!   on float leaves, matching what serde accepts), then recursively
//!   type-checks every value against its `LeafType`, enum-checks
//!   `LeafType::Enum`, and enforces required fields. Returns the merged
//!   value [`Map`] (the typed [`TypedBuilder`](crate::TypedBuilder)
//!   deserializes that map into `C` afterwards).
//!
//! [`Schema`]: crate::runtime::Schema
//! [`UnknownKey`]: crate::validate::UnknownKey

use crate::error::ClapfigError;
use crate::runtime::{Field, NamedField, Schema};
use crate::validate::UnknownKey;
use crate::value::{Map, Value};

/// Recursively walk `table` against `schema`, collecting dotted paths of
/// any keys not declared in the schema.
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
    unknown: &mut Vec<UnknownKey>,
) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match find_field(schema, key) {
            None => {
                // Capture the raw TOML key as the leaf — preserves quoted-
                // key semantics (`"acme.task-due-date-missing"` stays as a
                // single literal) so an `on_unknown_key` callback can
                // pattern-match on it (e.g. lex-fmt's "leaf contains a `.`
                // → accept" rule).
                unknown.push(UnknownKey {
                    path: full,
                    leaf: key.clone(),
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
                    collect_unknown_paths(t, nested, &full, unknown);
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
                            collect_unknown_paths(t, item_schema, &indexed, unknown);
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
                // `plugins` MapOf.
                if let Value::Map(entries) = value {
                    for (entry_key, entry_value) in entries {
                        if let Value::Map(t) = entry_value {
                            let entry_path = format!("{full}.{entry_key}");
                            collect_unknown_paths(t, item_schema, &entry_path, unknown);
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
pub(crate) fn fill_defaults_into(table: &mut Map, schema: &Schema) {
    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(leaf) => {
                if !table.contains_key(&nf.name) {
                    if let Some(default) = &leaf.default {
                        table.insert(nf.name.clone(), default.clone());
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
                        table.insert(nf.name.clone(), Value::Map(Map::new()));
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
                        table.insert(nf.name.clone(), Value::Array(Vec::new()));
                    }
                }
            }
            Field::Nested(nested) => {
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Map(Map::new()));
                if let Value::Map(t) = entry {
                    fill_defaults_into(t, nested);
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
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(items) = entry {
                    for item in items {
                        if let Value::Map(t) = item {
                            fill_defaults_into(t, item_schema);
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
                let entry = table
                    .entry(nf.name.clone())
                    .or_insert_with(|| Value::Map(Map::new()));
                if let Value::Map(entries) = entry {
                    for (_entry_key, entry_value) in entries.iter_mut() {
                        if let Value::Map(t) = entry_value {
                            fill_defaults_into(t, item_schema);
                        }
                    }
                }
            }
        }
    }
}

/// Finalize a merged table: apply schema-driven coercions
/// (datetime strings, integer-for-float), then enforce required fields
/// and per-leaf types. Returns the merged map on success.
pub(crate) fn finalize(mut merged: Map, schema: &Schema) -> Result<Map, ClapfigError> {
    coerce_leaf_values(&mut merged, schema);
    check_required_and_types(&merged, schema, "")?;
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
    schema: &Schema,
    prefix: &str,
) -> Result<(), ClapfigError> {
    for nf in &schema.fields {
        let path = if prefix.is_empty() {
            nf.name.clone()
        } else {
            format!("{prefix}.{}", nf.name)
        };
        match &nf.field {
            Field::Leaf(leaf) => match table.get(&nf.name) {
                None => {
                    if !leaf.optional {
                        return Err(ClapfigError::missing_required(path));
                    }
                }
                Some(value) => {
                    leaf.ty
                        .check(value)
                        .map_err(|reason| ClapfigError::invalid_value(path.clone(), reason))?;
                }
            },
            Field::Nested(nested) => match table.get(&nf.name) {
                None => {
                    // A nested section is required if any of its leaves is
                    // required. Recurse with an empty table so the missing-
                    // required check below fires for inner leaves.
                    let empty = Map::new();
                    check_required_and_types(&empty, nested, &path)?;
                }
                Some(Value::Map(inner)) => {
                    check_required_and_types(inner, nested, &path)?;
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value(
                        path,
                        format!("expected map, got {}", value_type_name(other)),
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
                        let indexed = format!("{path}[{i}]");
                        match item {
                            Value::Map(inner) => {
                                check_required_and_types(inner, item_schema, &indexed)?;
                            }
                            other => {
                                return Err(ClapfigError::invalid_value(
                                    indexed,
                                    format!("expected map, got {}", value_type_name(other)),
                                ));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value(
                        path,
                        format!("expected array, got {}", value_type_name(other)),
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
                        let entry_path = format!("{path}.{entry_key}");
                        match entry_value {
                            Value::Map(inner) => {
                                check_required_and_types(inner, item_schema, &entry_path)?;
                            }
                            other => {
                                return Err(ClapfigError::invalid_value(
                                    entry_path,
                                    format!("expected map, got {}", value_type_name(other)),
                                ));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::invalid_value(
                        path,
                        format!("expected map, got {}", value_type_name(other)),
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

    fn unknown_paths(table: &Map, schema: &Schema) -> Vec<String> {
        let mut unknown = Vec::new();
        collect_unknown_paths(table, schema, "", &mut unknown);
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

    // --- fill_defaults_into ---

    #[test]
    fn fill_defaults_populates_missing_top_level() {
        let mut table = parse("name = \"x\"\n");
        fill_defaults_into(&mut table, &test_schema());
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
        assert_eq!(table.get("level"), Some(&Value::String("info".into())));
    }

    #[test]
    fn fill_defaults_does_not_overwrite() {
        let mut table = parse("name = \"x\"\nport = 9999\n");
        fill_defaults_into(&mut table, &test_schema());
        assert_eq!(table.get("port"), Some(&Value::Integer(9999)));
    }

    #[test]
    fn fill_defaults_creates_nested_section_when_missing() {
        let mut table = parse("name = \"x\"\n");
        fill_defaults_into(&mut table, &test_schema());
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
        fill_defaults_into(&mut table, &schema);
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
        fill_defaults_into(&mut table, &schema);
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
        fill_defaults_into(&mut table, &schema);
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
        fill_defaults_into(&mut table, &schema);
        assert!(!table.contains_key("tags"));
    }

    // --- finalize: required-field check ---

    #[test]
    fn finalize_errors_on_missing_required() {
        let schema = test_schema();
        let mut table = parse("port = 1\n");
        fill_defaults_into(&mut table, &schema);
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
        fill_defaults_into(&mut table, &schema);
        let out = finalize(table, &schema).unwrap();
        assert_eq!(out.get("name"), Some(&Value::String("x".into())));
        assert_eq!(out.get("port"), Some(&Value::Integer(8080)));
    }

    // --- finalize: type check ---

    #[test]
    fn finalize_rejects_wrong_leaf_type() {
        let schema = test_schema();
        let mut table = parse("name = \"x\"\nport = \"oops\"\n");
        fill_defaults_into(&mut table, &schema);
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
        fill_defaults_into(&mut table, &schema);
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
}
