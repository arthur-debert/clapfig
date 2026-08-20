#[cfg(test)]
pub mod test {
    use crate::format::FormatAdapter;
    use crate::runtime::{Field, Schema};
    use crate::value::{Map, Value};

    /// Parse TOML source text into a value [`Map`] through the TOML
    /// adapter — the test-side stand-in for the discovery path, so unit
    /// tests never touch the `toml` crate directly.
    pub fn parse_toml(text: &str) -> Map {
        match crate::format::TomlAdapter
            .parse(text)
            .expect("test fixture TOML must parse")
            .value
        {
            Value::Map(map) => map,
            _ => unreachable!("TOML documents are maps at the root"),
        }
    }

    /// The canonical test schema shared across unit-test modules.
    ///
    /// Mirrors the shape the crate's docs use everywhere: a few scalar
    /// leaves plus one nested section with an optional leaf and a
    /// defaulted leaf.
    pub fn test_schema() -> Schema {
        Schema::object("TestConfig")
            .field(
                "host",
                Field::string()
                    .doc("The application host.")
                    .default("localhost"),
            )
            .field(
                "port",
                Field::integer().doc("The port number.").default(8080i64),
            )
            .field(
                "debug",
                Field::boolean().doc("Enable debug mode.").default(false),
            )
            .nested(
                "database",
                Schema::object("TestDbConfig")
                    .doc("Database settings.")
                    .field(
                        "url",
                        Field::string().doc("Connection string URL.").optional(),
                    )
                    .field(
                        "pool_size",
                        Field::integer().doc("Connection pool size.").default(5i64),
                    ),
            )
            .build()
    }

    /// Fixture for enum validation tests: a `mode` leaf constrained to
    /// `"fast" | "slow"` plus a plain integer leaf.
    pub fn enum_schema() -> Schema {
        Schema::object("EnumConfig")
            .field("mode", Field::enum_of(["fast", "slow"]).default("fast"))
            .field("port", Field::integer().default(8080i64))
            .build()
    }

    /// Fixture for kebab `strict_at` tests.
    ///
    /// Has a multi-word nested section so `normalize_keys(true)` +
    /// `strict_at("my-section", ...)` actually exercises the kebab → snake
    /// rewriter on the override path. `test_schema()`'s `database` is a
    /// single word and so wouldn't catch a normalization regression on its
    /// own.
    pub fn kebab_strict_at_schema() -> Schema {
        Schema::object("KebabStrictAtConfig")
            .nested(
                "my_section",
                Schema::object("KebabSection").field("x", Field::integer().optional()),
            )
            .build()
    }

    #[test]
    fn test_schema_loads_defaults() {
        let schema = test_schema();
        let mut table = Map::new();
        crate::schema_walk::fill_defaults_into(&mut table, &schema);
        let table =
            crate::schema_walk::finalize(table, &schema, &crate::error::DiscoveryRecord::empty())
                .unwrap();
        assert_eq!(table["host"].as_str(), Some("localhost"));
        assert_eq!(table["port"].as_integer(), Some(8080));
        assert_eq!(table["debug"].as_bool(), Some(false));
        let db = table["database"].as_map().unwrap();
        assert!(db.get("url").is_none());
        assert_eq!(db["pool_size"].as_integer(), Some(5));
    }
}
