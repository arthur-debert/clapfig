//! Runtime-path adapter: `DynamicSpec` implements [`ConfigSpec`] over an
//! owned [`Schema`].
//!
//! Pairs with [`crate::runtime`] (the owned schema data) and [`crate::spec`]
//! (the `SchemaRef` view that every other consumer walks). The adapter
//! walks the schema directly:
//!
//! - **`validate_unknown`**: recursive walk against the schema, every key
//!   not declared in the schema is collected and reported with line numbers
//!   from the same `find_key_line` heuristic the static path uses.
//! - **`fill_defaults`**: recursive walk, every missing leaf with a
//!   declared `default` is populated in place into the merged table.
//! - **`finalize`**: coerces schema-declared datetime leaves (string
//!   values in TOML's four lexical forms become [`Value::Datetime`] —
//!   schema-driven coercion per ADR-0001), then recursively type-checks
//!   every value against its `LeafType`, enum-checks `LeafType::Enum`,
//!   and enforces required fields. Returns the merged value [`Map`] (the
//!   typed `SchemaConfigBuilder` deserializes that map into `C`
//!   afterwards).
//!
//! [`ConfigSpec`]: crate::spec::ConfigSpec
//! [`Schema`]: crate::runtime::Schema

use std::path::Path;
use std::sync::Arc;

use crate::error::ClapfigError;
use crate::runtime::{Field, NamedField, Schema};
use crate::spec::{ConfigSpec, SchemaRef};
use crate::validate::{UnknownKey, ValidateContext, filter_through_cascade};
use crate::value::{Map, Value};

/// Runtime-path adapter: drives the resolve pipeline from a user-supplied
/// schema. The schema is held behind an `Arc` so the macro-driven static
/// path can share the same `&'static Schema` view that
/// `clapfig::Schema::schema()` caches without cloning the schema tree per
/// builder construction.
pub(crate) struct DynamicSpec {
    pub(crate) schema: Arc<Schema>,
}

impl DynamicSpec {
    pub fn new(schema: Schema) -> Self {
        Self {
            schema: Arc::new(schema),
        }
    }

    pub fn from_arc(schema: Arc<Schema>) -> Self {
        Self { schema }
    }
}

impl ConfigSpec for DynamicSpec {
    type Output = Map;

    fn schema(&self) -> SchemaRef<'_> {
        SchemaRef::from_dynamic(&self.schema)
    }

    fn validate_unknown(
        &self,
        table: &Map,
        source: &str,
        path: &Path,
        ctx: &ValidateContext<'_>,
    ) -> Result<Vec<crate::strict::CollectedUnknown>, ClapfigError> {
        let mut unknown: Vec<UnknownKey> = Vec::new();
        collect_unknown_paths(table, &self.schema, "", &mut unknown);
        filter_through_cascade(table, source, path, unknown, ctx)
    }

    fn fill_defaults(&self, table: &mut Map) -> Result<(), ClapfigError> {
        fill_defaults_into(table, &self.schema);
        Ok(())
    }

    fn finalize(&self, mut merged: Map) -> Result<Map, ClapfigError> {
        coerce_datetimes(&mut merged, &self.schema);
        check_required_and_types(&merged, &self.schema, "")?;
        Ok(merged)
    }
}

/// Recursively walk `table` against `schema`, collecting dotted paths of
/// any keys not declared in the schema.
///
/// For nested objects (`Field::Nested`) the recursion descends into the
/// sub-table; for `Field::ArrayOf`, each entry is validated against the
/// item schema; for `Field::MapOf`, each entry's value is validated
/// against the item schema (with the user-supplied key forming a path
/// segment).
fn collect_unknown_paths(
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
/// defaults. Existing values are never overwritten.
fn fill_defaults_into(table: &mut Map, schema: &Schema) {
    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(leaf) => {
                if !table.contains_key(&nf.name)
                    && let Some(default) = &leaf.default
                {
                    table.insert(nf.name.clone(), default.clone());
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
                // Array entries are user-supplied — only push defaults into
                // existing entries, never synthesize missing array items.
                if let Some(Value::Array(items)) = table.get_mut(&nf.name) {
                    for item in items {
                        if let Value::Map(t) = item {
                            fill_defaults_into(t, item_schema);
                        }
                    }
                }
            }
            Field::MapOf(item_schema) => {
                // Map entries are user-supplied — only push defaults into
                // existing entries, never synthesize missing map values.
                if let Some(Value::Map(entries)) = table.get_mut(&nf.name) {
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

/// Schema-driven datetime coercion (ADR-0001): for every leaf the schema
/// declares [`LeafType::DateTime`], a merged string value that parses as
/// one of TOML's four datetime lexical forms is replaced in place with the
/// typed [`Value::Datetime`]. Strings matching none of the forms are left
/// untouched, so the type check that follows reports the normal "expected
/// datetime, got string" error.
///
/// This is the seam that lets schema-blind sources — YAML/JSON files, env
/// vars, CLI/URL overrides — express datetimes as strings. Detection is
/// never value-sniffing: only leaves the schema declares as datetimes are
/// candidates.
fn coerce_datetimes(table: &mut Map, schema: &Schema) {
    for nf in &schema.fields {
        match &nf.field {
            Field::Leaf(leaf) => {
                if let Some(value) = table.get_mut(&nf.name) {
                    coerce_datetime_value(value, &leaf.ty);
                }
            }
            Field::Nested(nested) => {
                if let Some(Value::Map(t)) = table.get_mut(&nf.name) {
                    coerce_datetimes(t, nested);
                }
            }
            Field::ArrayOf(item_schema) => {
                if let Some(Value::Array(items)) = table.get_mut(&nf.name) {
                    for item in items {
                        if let Value::Map(t) = item {
                            coerce_datetimes(t, item_schema);
                        }
                    }
                }
            }
            Field::MapOf(item_schema) => {
                if let Some(Value::Map(entries)) = table.get_mut(&nf.name) {
                    for entry_value in entries.values_mut() {
                        if let Value::Map(t) = entry_value {
                            coerce_datetimes(t, item_schema);
                        }
                    }
                }
            }
        }
    }
}

/// Coerce one value against its declared leaf type, recursing through
/// declared containers (`Array` elements, homogeneous `Map` values).
/// Shared with the persist path, which validates `config set` values
/// against the same leaf declarations.
pub(crate) fn coerce_datetime_value(value: &mut Value, ty: &crate::runtime::LeafType) {
    use crate::runtime::LeafType;
    match ty {
        LeafType::DateTime => {
            if let Value::String(s) = value
                && let Ok(dt) = s.parse::<crate::value::Datetime>()
            {
                *value = Value::Datetime(dt);
            }
        }
        LeafType::Array(elem) => {
            if let Value::Array(items) = value {
                for item in items {
                    coerce_datetime_value(item, elem);
                }
            }
        }
        LeafType::Map(elem) => {
            if let Value::Map(entries) = value {
                for entry in entries.values_mut() {
                    coerce_datetime_value(entry, elem);
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
                        return Err(ClapfigError::MissingRequired { key: path });
                    }
                }
                Some(value) => {
                    leaf.ty
                        .check(value)
                        .map_err(|reason| ClapfigError::InvalidValue {
                            key: path.clone(),
                            reason,
                        })?;
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
                    return Err(ClapfigError::InvalidValue {
                        key: path,
                        reason: format!("expected map, got {}", value_type_name(other)),
                    });
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
                                return Err(ClapfigError::InvalidValue {
                                    key: indexed,
                                    reason: format!("expected map, got {}", value_type_name(other)),
                                });
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::InvalidValue {
                        key: path,
                        reason: format!("expected array, got {}", value_type_name(other)),
                    });
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
                                return Err(ClapfigError::InvalidValue {
                                    key: entry_path,
                                    reason: format!("expected map, got {}", value_type_name(other)),
                                });
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(ClapfigError::InvalidValue {
                        key: path,
                        reason: format!("expected map, got {}", value_type_name(other)),
                    });
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

    /// Default validate context — strict on, no overrides, no callback.
    /// Phase-3 trait method takes a `&ValidateContext<'_>`; tests reach
    /// for the same defaults via this `'static` helper.
    fn test_ctx() -> crate::validate::ValidateContext<'static> {
        use crate::strict::StrictnessOverrides;
        use std::sync::OnceLock;
        static EMPTY: OnceLock<StrictnessOverrides> = OnceLock::new();
        let overrides = EMPTY.get_or_init(StrictnessOverrides::new);
        crate::validate::ValidateContext {
            overrides,
            default_strict: true,
            callback: None,
            normalize_keys: false,
        }
    }

    // --- validate_unknown ---

    #[test]
    fn validate_unknown_accepts_known_keys() {
        let spec = DynamicSpec::new(test_schema());
        let table = parse("port = 1\nname = \"x\"\n");
        assert!(
            spec.validate_unknown(&table, "", std::path::Path::new("test"), &test_ctx())
                .is_ok()
        );
    }

    #[test]
    fn validate_unknown_flags_top_level_typo() {
        let spec = DynamicSpec::new(test_schema());
        let source = "name = \"x\"\ntypo = 1\n";
        let table = parse(source);
        let err = spec
            .validate_unknown(&table, source, std::path::Path::new("/t"), &test_ctx())
            .unwrap_err();
        let keys = err.unknown_keys().expect("unknown keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "typo");
        assert_eq!(keys[0].line, 2);
    }

    #[test]
    fn validate_unknown_flags_nested_typo() {
        let spec = DynamicSpec::new(test_schema());
        let source = "name = \"x\"\n[db]\ntypo = 1\n";
        let table = parse(source);
        let err = spec
            .validate_unknown(&table, source, std::path::Path::new("/t"), &test_ctx())
            .unwrap_err();
        let keys = err.unknown_keys().expect("unknown keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "db.typo");
    }

    // --- fill_defaults ---

    #[test]
    fn fill_defaults_populates_missing_top_level() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\n");
        spec.fill_defaults(&mut table).unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
        assert_eq!(table.get("level"), Some(&Value::String("info".into())));
    }

    #[test]
    fn fill_defaults_does_not_overwrite() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\nport = 9999\n");
        spec.fill_defaults(&mut table).unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(9999)));
    }

    #[test]
    fn fill_defaults_creates_nested_section_when_missing() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\n");
        spec.fill_defaults(&mut table).unwrap();
        let db = table.get("db").and_then(Value::as_map).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(5)));
        // `url` is optional; should stay absent.
        assert!(db.get("url").is_none());
    }

    // --- finalize: required-field check ---

    #[test]
    fn finalize_errors_on_missing_required() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("port = 1\n");
        spec.fill_defaults(&mut table).unwrap();
        let err = spec.finalize(table).unwrap_err();
        match err {
            ClapfigError::MissingRequired { key } => assert_eq!(key, "name"),
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn finalize_accepts_when_required_present() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\n");
        spec.fill_defaults(&mut table).unwrap();
        let out = spec.finalize(table).unwrap();
        assert_eq!(out.get("name"), Some(&Value::String("x".into())));
        assert_eq!(out.get("port"), Some(&Value::Integer(8080)));
    }

    // --- finalize: type check ---

    #[test]
    fn finalize_rejects_wrong_leaf_type() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\nport = \"oops\"\n");
        spec.fill_defaults(&mut table).unwrap();
        let err = spec.finalize(table).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason } => {
                assert_eq!(key, "port");
                assert!(reason.contains("expected integer"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn finalize_rejects_out_of_set_enum_value() {
        let spec = DynamicSpec::new(test_schema());
        let mut table = parse("name = \"x\"\nlevel = \"garbage\"\n");
        spec.fill_defaults(&mut table).unwrap();
        let err = spec.finalize(table).unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason } => {
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
        let spec = DynamicSpec::new(datetime_schema());
        for form in [
            "1979-05-27T07:32:00Z",
            "1979-05-27T07:32:00",
            "1979-05-27",
            "07:32:00",
        ] {
            let mut table = Map::new();
            table.insert("dt".into(), Value::String(form.into()));
            let out = spec.finalize(table).unwrap();
            match &out["dt"] {
                Value::Datetime(dt) => assert_eq!(dt.to_string(), form),
                other => panic!("{form}: expected Datetime, got {other:?}"),
            }
        }
    }

    #[test]
    fn finalize_coerces_datetimes_inside_declared_arrays() {
        let spec = DynamicSpec::new(datetime_schema());
        let mut table = Map::new();
        table.insert(
            "dts".into(),
            Value::Array(vec![Value::String("1979-05-27".into())]),
        );
        let out = spec.finalize(table).unwrap();
        match &out["dts"][0] {
            Value::Datetime(dt) => assert_eq!(dt.to_string(), "1979-05-27"),
            other => panic!("expected Datetime, got {other:?}"),
        }
    }

    #[test]
    fn finalize_rejects_malformed_datetime_string_as_type_error() {
        // A string matching none of the four forms is a normal type
        // error, never a silent pass-through.
        let spec = DynamicSpec::new(datetime_schema());
        for bad in ["not-a-date", "1979-13-45", "07:99:00", "2024/01/02"] {
            let mut table = Map::new();
            table.insert("dt".into(), Value::String(bad.into()));
            match spec.finalize(table) {
                Err(ClapfigError::InvalidValue { key, reason }) => {
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
        let spec = DynamicSpec::new(
            Schema::object("T")
                .field("s", RtField::string().optional())
                .build(),
        );
        let mut table = Map::new();
        table.insert("s".into(), Value::String("1979-05-27".into()));
        let out = spec.finalize(table).unwrap();
        assert_eq!(out["s"], Value::String("1979-05-27".into()));
    }

    #[test]
    fn finalize_nested_required_check() {
        // Supply every top-level required field by hand so the only missing
        // required field is `db.pool_size` (which has a default but is
        // required if no layer provides it). Skipping `fill_defaults`
        // exposes the nested-required path.
        let spec = DynamicSpec::new(test_schema());
        let table = parse("name = \"x\"\nport = 8080\nhost = \"h\"\nlevel = \"info\"\n[db]\n");
        let err = spec.finalize(table).unwrap_err();
        match err {
            ClapfigError::MissingRequired { key } => assert_eq!(key, "db.pool_size"),
            other => panic!("expected MissingRequired(db.pool_size), got {other:?}"),
        }
    }
}
