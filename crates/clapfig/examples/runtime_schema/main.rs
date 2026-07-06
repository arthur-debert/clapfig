//! # Runtime-schema demo
//!
//! Demonstrates [`Clapfig::runtime(schema)`](clapfig::Clapfig::runtime) — the
//! Phase 2 entry point for callers without a compile-time `Config` derive.
//! Builds a schema at runtime, loads layered config from file + env + a
//! programmatic CLI override, and exercises every `config gen|list|get|set`
//! action against the same schema.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example runtime_schema -- load
//! cargo run --example runtime_schema -- gen
//! cargo run --example runtime_schema -- get level
//! ```
//!
//! Set a config file in the working directory to override defaults:
//!
//! ```sh
//! cat > runtime-demo.toml <<EOF
//! port = 9090
//! level = "debug"
//!
//! [db]
//! url = "pg://prod"
//! EOF
//! cargo run --example runtime_schema -- load
//! ```
//!
//! Or env vars:
//!
//! ```sh
//! RUNTIME_DEMO__PORT=7777 cargo run --example runtime_schema -- load
//! ```

use clapfig::runtime::{Field, Schema};
use clapfig::types::{ConfigAction, SearchPath};
use clapfig::{Clapfig, RuntimeBuilder};

fn app_schema() -> Schema {
    Schema::object("RuntimeDemo")
        .doc("Demo app driven by a runtime-defined schema.")
        .field(
            "host",
            Field::string().doc("Bind address.").default("127.0.0.1"),
        )
        .field("port", Field::integer().doc("Bind port.").default(8080i64))
        .field(
            "level",
            Field::enum_of(["debug", "info", "warn", "error"])
                .doc("Log verbosity.")
                .default("info"),
        )
        .nested(
            "db",
            Schema::object("Db")
                .doc("Database settings.")
                .field("url", Field::string().doc("Connection URL.").optional())
                .field(
                    "pool_size",
                    Field::integer().doc("Connection pool size.").default(5i64),
                ),
        )
        .build()
}

fn make_builder() -> RuntimeBuilder {
    Clapfig::runtime(app_schema())
        .app_name("runtime-demo")
        .file_name("runtime-demo.toml")
        .search_paths(vec![SearchPath::Cwd, SearchPath::Platform])
        .persist_scope("local", SearchPath::Cwd)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "load".into());

    match arg.as_str() {
        "load" => {
            let table = make_builder().load()?;
            println!("Loaded config (as TOML):");
            println!("{}", toml::to_string_pretty(&table)?);
        }
        "gen" => {
            make_builder().handle_and_print(&ConfigAction::Gen { output: None })?;
        }
        "schema" => {
            make_builder().handle_and_print(&ConfigAction::Schema { output: None })?;
        }
        "list" => {
            make_builder().handle_and_print(&ConfigAction::List { scope: None })?;
            println!();
        }
        "get" => {
            let key = std::env::args()
                .nth(2)
                .ok_or("usage: runtime_schema get <key>")?;
            make_builder().handle_and_print(&ConfigAction::Get { key, scope: None })?;
            println!();
        }
        "set" => {
            let key = std::env::args()
                .nth(2)
                .ok_or("usage: runtime_schema set <key> <value>")?;
            let value = std::env::args()
                .nth(3)
                .ok_or("usage: runtime_schema set <key> <value>")?;
            make_builder().handle_and_print(&ConfigAction::Set {
                key,
                value,
                scope: None,
            })?;
            println!();
        }
        "unset" => {
            let key = std::env::args()
                .nth(2)
                .ok_or("usage: runtime_schema unset <key>")?;
            make_builder().handle_and_print(&ConfigAction::Unset { key, scope: None })?;
            println!();
        }
        other => {
            eprintln!("Unknown action: {other}");
            eprintln!(
                "Try: load | gen | schema | list | get <key> | set <key> <value> | unset <key>"
            );
            std::process::exit(2);
        }
    }
    Ok(())
}
