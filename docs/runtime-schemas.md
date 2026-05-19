# Runtime-defined schemas

Some apps don't have a single compile-time `Config` struct. Plugin hosts
assemble their schema from loaded plugins. Scripting hosts read it from
a config descriptor file. Generated apps build it programmatically. For
those cases, clapfig exposes a runtime-schema entry point next to the
static `Clapfig::builder::<C>()` one.

## When to reach for it

Use `Clapfig::runtime(schema)` when **the set of valid keys is not known
at compile time**. Examples:

- A linter whose rule keys come from a directory of plugin manifests.
- A formatter that ships per-language config schemas loaded from
  `LANG/clapfig.toml` files at startup.
- An embedded scripting host where each script declares its own
  config shape.
- A test fixture that constructs a schema inline to exercise a specific
  combination of fields without writing a Rust type for it.

If your config schema is known at compile time, use
`Clapfig::builder::<C>()` instead — the static path is simpler, gives you
typed access to the result, and benefits from confique's `derive(Config)`
ergonomics.

## Building a schema

```rust,ignore
use clapfig::runtime::{Field, Schema};

let schema = Schema::object("App")
    .doc("Top-level application config.")
    .field("host", Field::string().doc("App host.").default("localhost"))
    .field("port", Field::integer().default(8080i64))
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
            .field("url", Field::string().optional())
            .field("pool_size", Field::integer().default(5i64)),
    )
    .build();
```

### Field kinds

- **`Field::string()`, `Field::integer()`, `Field::float()`, `Field::boolean()`, `Field::datetime()`** — TOML primitive leaves.
- **`Field::array_of_type(LeafType)`** — homogeneous array of a primitive type.
- **`Field::map_of(LeafType)`** — string-keyed map with homogeneous values.
- **`Field::enum_of(values)`** — constrained value: must be one of the listed TOML primitives. Used for log levels, output formats, modes.
- **`Schema::object(...).nested(name, child)`** — TOML `[section]` sub-object.
- **`Schema::object(...).array_of(name, item_schema)`** — TOML `[[name]]` array of sub-objects.

### Per-leaf modifiers

`.doc(line)` appends a doc-comment line (multiple calls accumulate).
`.default(value)` sets a default. `.optional()` marks the leaf as
optional (otherwise required-after-merge produces
`ClapfigError::MissingRequired`). `.env(name)` overrides the env-var name.

### Field-name validation

Field names are validated at `SchemaBuilder` time. Names containing `.`,
`[`, or `]` (which collide with dotted-path / array-index syntax),
empty names, and duplicate names within one schema panic at the call
site rather than producing silent `KeyNotFound` errors at every
consumer.

## Using a schema

`Clapfig::runtime(schema)` returns a `RuntimeBuilder` with the same
surface as `Clapfig::builder::<C>()`:

```rust,ignore
use clapfig::{Clapfig, types::SearchPath};

let table: toml::Table = Clapfig::runtime(schema)
    .app_name("myapp")
    .file_name("myapp.toml")
    .search_paths(vec![SearchPath::Cwd, SearchPath::Platform])
    .persist_scope("local", SearchPath::Cwd)
    .load()?;
```

Differences from the static path:

- `load()` returns `toml::Table`, not a typed struct.
- `post_validate` receives `&Table` instead of `&C`.
- `RuntimeResolver` (returned by `build_resolver()`) parallels
  `Resolver<C>` for tree-walk use cases.

Everything else — `search_paths`, `search_mode`, `env_prefix`,
`cli_override`, `cli_overrides_from`, `url_query`, `normalize_keys`,
`layer_order`, `strict`, `strict_at`, `on_unknown_key`, `handle`,
`handle_and_print`, `handle_to_string` — works identically.

## Subcommand support

`RuntimeBuilder::handle(&ConfigAction)` drives the same
`config gen|list|get|set|unset|schema` actions the static path
supports. Doc comments and enum allowed-value lists are read straight
off the schema:

- `config gen` renders a commented template; enum leaves get
  `# Allowed: "debug" | "info" | "warn" | "error"`.
- `config schema` emits a JSON Schema document with the same enum
  values as `enum: [...]` on the property.
- `meta::doc_for_runtime(&schema, "db.pool_size")` reads doc-comment
  lines from the schema the same way `meta::doc_for::<C>(...)` reads
  them from `C::META`.

## What's not yet supported

- `deserialize_with`-style normalizers on runtime leaves.
- Mixing a runtime sub-schema inside a static `Config` struct.
- Indexed dotted-key syntax (`plugins[0].id`) for `config set` on
  arrays of objects.

See the proposal in issue #38 for context on the deferred items.

## Example

A runnable example lives at
[`examples/runtime_schema/`](https://github.com/arthur-debert/clapfig/tree/main/examples/runtime_schema).
Run with `cargo run --example runtime_schema -- load`.
