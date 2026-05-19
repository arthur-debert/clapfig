//! Runtime-path adapter: `DynamicSpec` implements [`ConfigSpec`] over an
//! owned [`Schema`].
//!
//! Pairs with [`crate::runtime`] (the owned schema data) and [`crate::spec`]
//! (the `SchemaRef` view that every other consumer walks). The static path's
//! `StaticSpec<C>` delegates to confique; this adapter walks the schema
//! directly:
//!
//! - **`validate_unknown`**: recursive walk against the schema, every key
//!   not declared in the schema is collected and reported with line numbers
//!   from the same `find_key_line` heuristic the static path uses.
//! - **`fill_defaults`**: recursive walk, every missing leaf with a
//!   declared `default` is populated in place into the merged table.
//! - **`finalize`**: recursive walk, type-checks every value against its
//!   `LeafType`, enum-checks `LeafType::Enum`, enforces required fields.
//!   Returns the merged `toml::Table` unchanged (the runtime path produces
//!   raw TOML rather than a typed struct).
//!
//! [`ConfigSpec`]: crate::spec::ConfigSpec
//! [`Schema`]: crate::runtime::Schema

use std::path::Path;

use toml::{Table, Value};

use crate::error::ClapfigError;
use crate::runtime::{Field, NamedField, Schema};
use crate::spec::{ConfigSpec, SchemaRef};
use crate::validate::{UnknownKey, ValidateContext, filter_through_cascade};

/// Runtime-path adapter: drives the resolve pipeline from an owned
/// user-supplied schema.
pub(crate) struct DynamicSpec {
    pub(crate) schema: Schema,
}

impl DynamicSpec {
    pub fn new(schema: Schema) -> Self {
        Self { schema }
    }
}

impl ConfigSpec for DynamicSpec {
    type Output = Table;

    fn schema(&self) -> SchemaRef<'_> {
        SchemaRef::from_dynamic(&self.schema)
    }

    fn validate_unknown(
        &self,
        table: &Table,
        source: &str,
        path: &Path,
        ctx: &ValidateContext<'_>,
    ) -> Result<(), ClapfigError> {
        let mut unknown: Vec<UnknownKey> = Vec::new();
        collect_unknown_paths(table, &self.schema, "", &mut unknown);
        filter_through_cascade(table, source, path, unknown, ctx)
    }

    fn fill_defaults(&self, table: &mut Table) -> Result<(), ClapfigError> {
        fill_defaults_into(table, &self.schema);
        Ok(())
    }

    fn finalize(&self, merged: Table) -> Result<Table, ClapfigError> {
        check_required_and_types(&merged, &self.schema, "")?;
        Ok(merged)
    }
}

/// Recursively walk `table` against `schema`, collecting dotted paths of
/// any keys not declared in the schema.
///
/// For nested objects (`Field::Nested`) the recursion descends into the
/// sub-table; for `Field::ArrayOf`, each entry is validated against the
/// item schema.
fn collect_unknown_paths(
    table: &Table,
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
                if let Value::Table(t) = value {
                    collect_unknown_paths(t, nested, &full, unknown);
                }
            }
            Some(NamedField {
                field: Field::ArrayOf(item_schema),
                ..
            }) => {
                if let Value::Array(items) = value {
                    for (i, item) in items.iter().enumerate() {
                        if let Value::Table(t) = item {
                            let indexed = format!("{full}[{i}]");
                            collect_unknown_paths(t, item_schema, &indexed, unknown);
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
fn fill_defaults_into(table: &mut Table, schema: &Schema) {
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
                    .or_insert_with(|| Value::Table(Table::new()));
                if let Value::Table(t) = entry {
                    fill_defaults_into(t, nested);
                }
            }
            Field::ArrayOf(item_schema) => {
                // Array entries are user-supplied — only push defaults into
                // existing entries, never synthesize missing array items.
                if let Some(Value::Array(items)) = table.get_mut(&nf.name) {
                    for item in items {
                        if let Value::Table(t) = item {
                            fill_defaults_into(t, item_schema);
                        }
                    }
                }
            }
        }
    }
}

/// Recursively validate required-field presence and per-leaf types.
fn check_required_and_types(
    table: &Table,
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
                    let empty = Table::new();
                    check_required_and_types(&empty, nested, &path)?;
                }
                Some(Value::Table(inner)) => {
                    check_required_and_types(inner, nested, &path)?;
                }
                Some(other) => {
                    return Err(ClapfigError::InvalidValue {
                        key: path,
                        reason: format!("expected table, got {}", value_type_name(other)),
                    });
                }
            },
            Field::ArrayOf(item_schema) => match table.get(&nf.name) {
                None => {
                    // Absent array-of: empty list is the natural default,
                    // not an error. Matches confique's behavior for
                    // `Vec<Nested>`-style fields.
                }
                Some(Value::Array(items)) => {
                    for (i, item) in items.iter().enumerate() {
                        let indexed = format!("{path}[{i}]");
                        match item {
                            Value::Table(inner) => {
                                check_required_and_types(inner, item_schema, &indexed)?;
                            }
                            other => {
                                return Err(ClapfigError::InvalidValue {
                                    key: indexed,
                                    reason: format!(
                                        "expected table, got {}",
                                        value_type_name(other)
                                    ),
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
        }
    }
    Ok(())
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "table",
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

    fn parse(toml_text: &str) -> Table {
        toml_text.parse().unwrap()
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
        let db = table.get("db").and_then(Value::as_table).unwrap();
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
