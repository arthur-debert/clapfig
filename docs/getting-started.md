# Getting Started

This guide walks you through adding clapfig to a Rust project and loading your
first layered configuration.

## Installation

Add clapfig to your `Cargo.toml`:

```toml
[dependencies]
clapfig = "0.23"
```

This pulls in the `clap` feature by default, which gives you the `config`
subcommand integration, and the `derive` feature for
`#[derive(clapfig::Schema)]`. If you don't use clap:

```toml
[dependencies]
clapfig = { version = "0.23", default-features = false, features = ["derive"] }
```

## Define your config struct

Clapfig's `Schema` derive turns a plain Rust struct into a layered
configuration schema:

```rust
use clapfig::Schema;
use serde::{Serialize, Deserialize};

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct AppConfig {
    /// The host address to bind to.
    #[clapfig(default = "127.0.0.1")]
    pub host: String,

    /// The port number.
    #[clapfig(default = 8080)]
    pub port: u16,

    /// Enable debug mode.
    #[clapfig(default = false)]
    pub debug: bool,

    /// Database settings.
    pub database: DbConfig,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct DbConfig {
    /// Connection string URL.
    pub url: Option<String>,

    /// Connection pool size.
    #[clapfig(default = 10)]
    pub pool_size: usize,
}
```

Key points:

- **`#[clapfig(default = ...)]`** sets the compiled default — the lowest
  layer, always present. Works with scalars, strings, and collections
  (`default = []` for an empty vec).
- **Nested structs** — a field whose type also derives `Schema` becomes a
  TOML section, addressable via dotted keys and `__` env var separators.
- **Unit-only enums** deriving `Schema` become constrained value sets:
  out-of-set values error at load, and generated templates carry an
  `# Allowed: ...` line.
- **`Option<T>`** fields are truly optional — omitting them everywhere is
  valid. Non-optional fields without a default must be provided by at least
  one layer.
- **`///` doc comments** are used in generated templates and `config get`
  output.
- The struct still derives serde's `Serialize`/`Deserialize` — clapfig uses
  them for the final typed deserialize, so value-shaping serde attributes
  (`#[serde(deserialize_with = ...)]` etc.) apply as usual. Field *naming*
  is shared with the schema: `#[serde(rename = "...")]` on a field renames
  the config key too (the schema follows serde's spelling; the directional
  `rename(deserialize = "...")` form contributes its deserialize name),
  while struct-level `#[serde(rename_all = ...)]` — directional included —
  is rejected at compile time — rename fields individually.

## Load it

```rust
use clapfig::Clapfig;

fn main() -> anyhow::Result<()> {
    let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .load()?;

    println!("Listening on {}:{}", config.host, config.port);
    Ok(())
}
```

That `app_name("myapp")` call sets sensible defaults:

- Searches for `myapp.toml` in the platform config directory
- Merges env vars prefixed with `MYAPP__`
- Fills in `#[clapfig(default)]` values for anything not provided

## Override from the environment

With prefix `MYAPP`, environment variables map through double-underscore
nesting:

| Env var                | Config key       |
|------------------------|------------------|
| `MYAPP__HOST`          | `host`           |
| `MYAPP__DATABASE__URL` | `database.url`   |

```sh
MYAPP__DATABASE__URL=postgres://localhost/mydb cargo run
```

Disable env loading with `.no_env()` when you don't want it:

```rust
let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
    .app_name("myapp")
    .no_env()
    .load()?;
```

## Add search paths

Control where clapfig looks for config files:

```rust
use clapfig::{Clapfig, SearchPath};

let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
    .app_name("myapp")
    .search_paths(vec![
        SearchPath::Platform,             // XDG / Library / AppData
        SearchPath::Home(".myapp"),        // ~/.myapp/
        SearchPath::Cwd,                  // current directory
    ])
    .load()?;
```

Paths are listed in priority-ascending order — later paths override earlier
ones. Missing files are silently skipped.

## Add clap integration

With the `clap` feature (on by default), embed `ConfigArgs` in your CLI to get
`config gen|list|get|set|unset` for free:

```rust
use clap::{Parser, Subcommand};
use clapfig::{Clapfig, ConfigArgs, SearchPath};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the app.
    Run,
    /// Manage configuration.
    Config(ConfigArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let builder = Clapfig::schema_builder::<AppConfig>()
        .app_name("myapp")
        .persist_scope("local", SearchPath::Cwd);

    match cli.command {
        Commands::Run => {
            let config = builder.load()?;
            println!("Running on port {}", config.port);
        }
        Commands::Config(args) => {
            builder.handle_and_print(&args.into_action())?;
        }
    }
    Ok(())
}
```

This gives your users:

```sh
myapp config gen              # print a commented TOML template
myapp config list             # show all resolved values
myapp config get server.port  # show a single key with its doc comment
myapp config set port 9090    # persist a value to the config file
myapp config unset port       # remove a persisted value
```

## Strict mode

Strict mode is **on by default**. If a config file contains a key that doesn't
match any field in your struct, loading fails with the file path, key name, and
line number:

```text
Unknown key 'typo_key' in /home/user/.config/myapp/myapp.toml (line 5)
```

Turn it off with `.strict(false)` if you share config files across tools.

## Next steps

- [Layered Configuration](./layered-config.md) — deep dive into layers,
  search modes, and merge behavior.
- [Runtime Schemas](./runtime-schemas.md) — building schemas at runtime for
  plugin hosts and generated apps.
- [Resolver Guide](./resolver.md) — per-directory config resolution for
  tree-walk tools.
- [Config Command Guide](./config-command.md) — the full `config`
  subcommand integration.
