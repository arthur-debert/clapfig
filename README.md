# clapfig

Rich, layered configuration for Rust applications. Define a struct, point at your files, and go.

**clapfig** discovers, merges, and manages configuration from multiple sources — config files, environment variables, and programmatic overrides — through a pure Rust builder API. The core library has **no dependency on any CLI framework**: you can use it in GUI apps, servers, or with any argument parser. For [clap](https://docs.rs/clap) users, an optional adapter provides drop-in `config gen|list|get|set` subcommands with zero boilerplate.

Built on [confique](https://github.com/LukasKalbertodt/confique) for struct-driven defaults and commented template generation.

## Features

**Core** (always available, no CLI framework needed):

- **Struct as source of truth** — define settings as a Rust struct with defaults and `///` doc comments
- **Layered merge** — defaults < config files < env vars < overrides, every layer sparse
- **Multi-path file search** — platform config dir, home, cwd, ancestor walk, or any path, in precedence order
- **Search modes** — merge all found configs (layered overrides) or use the first match ("find my config")
- **Ancestor walk** — `SearchPath::Ancestors` walks up from cwd to find project configs, with configurable boundary (`.git`, filesystem root)
- **Prefix-based env vars** — `MYAPP__DATABASE__URL` maps to `database.url` automatically
- **Strict mode** — unknown keys in config files error with file path, key name, and line number (on by default)
- **Template generation** — emit a commented sample config derived from the struct's doc comments
- **Persistence with named scopes** — `persist_scope("local", path)` / `persist_scope("global", path)` for global/local config patterns. Scope paths auto-added to search paths.

**Clap adapter** (`clap` feature, on by default):

- **Config subcommand** — drop-in `config gen|get|set|list` commands for clap
- **`--scope` flag** — target a specific scope for any config subcommand (e.g. `config set key val --scope global`)
- **Auto-matching overrides** — map clap args to config keys by name in one call

## Quick Start

Define your config with confique's `Config` derive:

```rust
use confique::Config;
use serde::{Serialize, Deserialize};

#[derive(Config, Serialize, Deserialize, Debug)]
pub struct AppConfig {
    /// The host address to bind to.
    #[config(default = "127.0.0.1")]
    pub host: String,

    /// The port number.
    #[config(default = 8080)]
    pub port: u16,

    /// Database settings.
    #[config(nested)]
    pub database: DbConfig,
}

#[derive(Config, Serialize, Deserialize, Debug)]
pub struct DbConfig {
    /// Connection string URL.
    pub url: Option<String>,

    /// Connection pool size.
    #[config(default = 10)]
    pub pool_size: usize,
}
```

Load it in one line:

```rust
use clapfig::Clapfig;

fn main() -> anyhow::Result<()> {
    let config: AppConfig = Clapfig::builder()
        .app_name("myapp")
        .load()?;

    println!("Listening on {}:{}", config.host, config.port);
    Ok(())
}
```

That `app_name("myapp")` call sets sensible defaults:

- Searches for `myapp.toml` in the platform config directory
- Merges env vars prefixed with `MYAPP__`
- Fills in `#[config(default)]` values for anything not provided

## Setup

### Defaults from `app_name`

| Derived setting | Value |
|-----------------|-------|
| File name | `{app_name}.toml` |
| Search paths | Platform config dir (via [`directories`](https://docs.rs/directories)) |
| Env prefix | `{APP_NAME}` (uppercased) |

### Builder methods

```rust
use clapfig::{Clapfig, SearchPath};

let config: AppConfig = Clapfig::builder()
    // Required — sets defaults for file_name, search_paths, env_prefix
    .app_name("myapp")

    // Optional overrides
    .file_name("settings.toml")                                   // override config file name
    .search_paths(vec![SearchPath::Platform, SearchPath::Cwd])    // replace default search paths
    .add_search_path(SearchPath::Cwd)                             // append a path without replacing
    .env_prefix("MY_APP")                                         // override env var prefix
    .no_env()                                                     // disable env var loading entirely
    .strict(false)                                                // disable strict mode (allow unknown keys)
    .cli_override("host", some_value)                             // override a key from a CLI arg
    .load()?;
```

### Search Paths, Modes, and Persistence

Config file handling has three orthogonal axes on the builder:

- **Discovery** (`.search_paths()`) — where to look. Paths listed in priority-ascending order (last = highest).
- **Resolution** (`.search_mode()`) — `Merge` (default: deep-merge all found files) or `FirstMatch` (use the single highest-priority file).
- **Persistence** (`.persist_scope(name, path)`) — named targets for `config set` writes. Scope paths are auto-added to search paths.

```rust
use clapfig::{Clapfig, SearchPath, SearchMode, Boundary};

// Layered global + local with named scopes
let config: AppConfig = Clapfig::builder()
    .app_name("myapp")
    .search_paths(vec![SearchPath::Platform, SearchPath::Cwd])
    .persist_scope("local", SearchPath::Cwd)        // default for writes
    .persist_scope("global", SearchPath::Platform)
    .load()?;

// Find nearest project config (walk up to .git, use first match)
let config: AppConfig = Clapfig::builder()
    .app_name("mytool")
    .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
    .search_mode(SearchMode::FirstMatch)
    .load()?;
```

With scopes configured, the `--scope` flag targets specific config files:

```sh
myapp config set port 3000                    # writes to "local" (default)
myapp config set port 3000 --scope global     # writes to "global"
myapp config list                             # merged resolved config
myapp config list --scope global              # only global scope's entries
```

Available `SearchPath` variants: `Platform`, `Home(".myapp")`, `Cwd`, `Path(PathBuf)`, `Ancestors(Boundary)`.

`Ancestors` walks up from cwd, expanding inline into multiple directories (shallowest first, cwd last = highest priority). `Boundary::Root` walks to the filesystem root; `Boundary::Marker(".git")` stops at the directory containing the marker (inclusive).

Missing files are silently skipped. See the [`types` module docs](https://docs.rs/clapfig/latest/clapfig/types/) for the full conceptual guide and use-case examples.

### Strict Mode

Strict mode is **on by default**. If a config file contains a key that doesn't match any field in your struct, loading fails with a clear error including the file path, key name, and line number:

```
Unknown key 'typo_key' in /home/user/.config/myapp/myapp.toml (line 5)
```

Disable it with `.strict(false)` if you want to allow extra keys.

## Normalizing Values

You can normalize config values during deserialization using confique's `#[config(deserialize_with = ...)]` attribute. The function has the standard serde deserializer signature and runs automatically when a value is loaded from any source — config files, environment variables, or programmatic overrides.

```rust
use confique::Config;
use serde::{Serialize, Deserialize, Deserializer};

/// Normalize a string to lowercase during deserialization.
fn normalize_lowercase<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Ok(s.to_lowercase())
}

#[derive(Config, Serialize, Deserialize, Debug)]
pub struct DisplayConfig {
    /// Terminal color name, always stored as lowercase.
    #[config(deserialize_with = normalize_lowercase, default = "yellow")]
    pub color: String,

    /// Output format (pretty or plain).
    #[config(default = "pretty")]
    pub format: String,
}
```

With this, `color = "BLUE"` in a TOML file, `MYAPP__COLOR=Blue` as an env var, or `.cli_override("color", "RED")` all resolve to their lowercase form. Note that `#[config(default)]` values are injected directly by confique and do **not** pass through the deserializer — if your default needs normalization, write it in normalized form.

## Environment Variables

With env prefix `MYAPP`, variables map via double-underscore nesting:

| Env var | Config key |
|---------|------------|
| `MYAPP__HOST` | `host` |
| `MYAPP__PORT` | `port` |
| `MYAPP__DATABASE__URL` | `database.url` |
| `MYAPP__DATABASE__POOL_SIZE` | `database.pool_size` |

`__` (double underscore) separates nesting levels. Single `_` within a segment is literal (part of the field name).

Disable env loading entirely with `.no_env()`.

## Programmatic Overrides

The `cli_override` and `cli_overrides_from` methods on the builder work with **any** value source — they are not clap-specific despite the name. Use them to inject overrides from CLI args, GUI inputs, HTTP requests, or anything else.

### Auto-matching

If your override source derives `Serialize`, `cli_overrides_from` auto-matches fields by name against config keys:

```rust
use clap::Parser;
use serde::Serialize;

#[derive(Parser, Serialize)]
struct Cli {
    #[command(subcommand)]
    #[serde(skip)]
    command: Commands,

    #[arg(long)]
    host: Option<String>,

    #[arg(long)]
    port: Option<i64>,

    #[arg(long)]
    db_url: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config: AppConfig = Clapfig::builder()
        .app_name("myapp")
        .cli_overrides_from(&cli)                // auto-matches host, port
        .cli_override("database.url", cli.db_url) // manual: name doesn't match
        .load()?;

    Ok(())
}
```

`cli_overrides_from(source)` serializes the source, skips `None` values, and keeps only keys that match a config field. Non-matching fields (`command`, `db_url`) are silently ignored. Works with any `Serialize` type — structs, `HashMap`s, etc.

#### Manual overrides

For fields where the CLI name differs from the config key, use `cli_override`:

```rust
.cli_override("database.url", cli.db_url)
```

`cli_override(key, value)` takes `Option<V>` where `V: Into<toml::Value>` — `None` is silently skipped. Dot notation addresses nested keys.

Both methods compose freely and push to the same override list. Later calls take precedence.

Supported value types: `String`, `&str`, `i64`, `i32`, `i8`, `u8`, `u32`, `f64`, `f32`, `bool`.

## Clap Adapter (optional)

> Requires the `clap` Cargo feature (enabled by default). To use clapfig without clap:
> ```toml
> clapfig = { version = "...", default-features = false }
> ```

### Config Subcommand

Add config management to your CLI by nesting `clapfig::ConfigArgs`:

```rust
use clap::Subcommand;
use clapfig::{Clapfig, ConfigArgs, ConfigResult};

#[derive(Subcommand)]
enum Commands {
    /// Run the application
    Run,
    /// Manage configuration
    Config(ConfigArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Config(args) => {
            let action = args.into_action();
            let result = Clapfig::builder::<AppConfig>()
                .app_name("myapp")
                .handle(&action)?;
            match result {
                ConfigResult::Template(t) => print!("{t}"),
                ConfigResult::KeyValue { key, value, doc } => {
                    for line in &doc { println!("# {line}"); }
                    println!("{key} = {value}");
                }
                ConfigResult::ValueSet { key, value } => {
                    println!("Set {key} = {value}");
                }
            }
        }
        Commands::Run => {
            let config: AppConfig = Clapfig::builder()
                .app_name("myapp")
                .cli_override("host", cli.host)
                .cli_override("port", cli.port)
                .load()?;
            println!("Listening on {}:{}", config.host, config.port);
        }
    }

    Ok(())
}
```

This gives your users:

```sh
myapp config gen                    # print commented sample config to stdout
myapp config gen -o myapp.toml      # write to file
myapp config get database.url       # print the resolved value of a key
myapp config set port 3000          # persist a value to the user's config file
```

### Custom Command Names

If the default subcommand or flag names conflict with your app (e.g. you already have a global `--scope` flag), use `ConfigCommand` to rename anything:

```rust
use clap::{Command, Parser, Subcommand};
use clapfig::{Clapfig, ConfigCommand};

#[derive(Parser)]
struct Cli {
    #[arg(long, global = true)]
    scope: Option<String>,          // conflicts with ConfigArgs' --scope

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
}

fn main() -> anyhow::Result<()> {
    let config_cmd = ConfigCommand::new()
        .scope_long("target")           // --scope → --target
        .gen_name("template");          // gen → template

    let app = Cli::command()
        .subcommand(config_cmd.as_command("settings"));

    let matches = app.get_matches();

    if let Some(("settings", sub)) = matches.subcommand() {
        let action = config_cmd.parse(sub)?;
        Clapfig::builder::<AppConfig>()
            .app_name("myapp")
            .handle_and_print(&action)?;
        return Ok(());
    }

    let cli = Cli::from_arg_matches(&matches)?;
    // handle other commands...
    Ok(())
}
```

Available builder methods: `list_name()`, `gen_name()`, `get_name()`, `set_name()`, `unset_name()`, `scope_long()`, `output_long()`, `output_short()`. Default names match `ConfigArgs` exactly, so `ConfigCommand::new()` with no customization is equivalent to the derive path.

## Template Generation

`config gen` produces a commented TOML file derived from your struct's `///` doc comments:

```toml
# The host address to bind to.
# Default: "127.0.0.1"
#host = "127.0.0.1"

# The port number.
# Default: 8080
#port = 8080

[database]
# Connection string URL.
#url =

# Connection pool size.
# Default: 10
#pool_size = 10
```

The template stays in sync with code — it's generated from the same struct. Change a doc comment or a default, the template reflects it.

## Layer Precedence

```
Compiled defaults     #[config(default = ...)]
       ↑ overridden by
Config files          search paths in order, later paths win
       ↑ overridden by
Environment vars      MYAPP__KEY
       ↑ overridden by
Overrides             .cli_override()
```

Every layer is **sparse**. You only specify the keys you want to override. Unset keys fall through to the next layer down.

## Persistence

`config set <key> <value>` writes to a named persist scope configured via `.persist_scope()` on the builder.

- **Named scopes** — each scope has a name (e.g. "local", "global") and a `SearchPath`. The first scope added is the default for writes.
- **Auto-discovery** — scope paths are automatically added to search paths, so persisted values are always discoverable in the merged view.
- **`--scope` flag** — target a specific scope: `config set key val --scope global`. Works with `list`, `get`, `set`, and `unset`.
- If the file exists, the key is patched in place using [`toml_edit`](https://docs.rs/toml_edit), **preserving existing comments and formatting**.
- If the file doesn't exist, a fresh config is created from the generated template with the target key set.
- If no scopes are configured, `config set` returns `ClapfigError::NoPersistPath`.

## Demo Application

The repo includes a runnable example that exercises every clapfig feature — nested config structs, file search paths, env vars, CLI overrides, and the `config` subcommand. It's a good starting point for integration and for ad-hoc testing.

```sh
# Print all resolved values (default color: yellow)
cargo run --example clapfig_demo -- echo

# Override via env var
CLAPFIG_DEMO__DISPLAY__COLOR=red cargo run --example clapfig_demo -- echo

# Override via CLI flag
cargo run --example clapfig_demo -- --color blue --port 8080 echo

# Config subcommands
cargo run --example clapfig_demo -- config gen
cargo run --example clapfig_demo -- config list
cargo run --example clapfig_demo -- config get server.port
```

See [`examples/clapfig_demo/`](examples/clapfig_demo/) for the full source.

## Full Example (with clap)

```rust
use clap::{Parser, Subcommand};
use confique::Config;
use serde::{Serialize, Deserialize};
use clapfig::{Clapfig, ConfigArgs, ConfigResult, SearchPath};

// -- Config struct --

#[derive(Config, Serialize, Deserialize, Debug)]
pub struct AppConfig {
    /// The host address to bind to.
    #[config(default = "127.0.0.1")]
    pub host: String,

    /// The port number.
    #[config(default = 8080)]
    pub port: u16,

    /// Database settings.
    #[config(nested)]
    pub database: DbConfig,
}

#[derive(Config, Serialize, Deserialize, Debug)]
pub struct DbConfig {
    /// Connection string URL.
    pub url: Option<String>,

    /// Connection pool size.
    #[config(default = 10)]
    pub pool_size: usize,
}

// -- CLI --

#[derive(Parser, Serialize)]
#[command(name = "myapp")]
struct Cli {
    #[command(subcommand)]
    #[serde(skip)]
    command: Commands,

    #[arg(long, global = true)]
    host: Option<String>,

    #[arg(long, global = true)]
    port: Option<i64>,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    Config(ConfigArgs),
}

// -- Main --

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Config(args) => {
            Clapfig::builder::<AppConfig>()
                .app_name("myapp")
                .add_search_path(SearchPath::Cwd)
                .persist_scope("local", SearchPath::Cwd)
                .handle_and_print(&args.into_action())?;
        }
        Commands::Run => {
            let config: AppConfig = Clapfig::builder()
                .app_name("myapp")
                .add_search_path(SearchPath::Cwd)
                .cli_overrides_from(&cli)
                .load()?;

            println!("Listening on {}:{}", config.host, config.port);
            if let Some(url) = &config.database.url {
                println!("Database: {}", url);
            }
        }
    }

    Ok(())
}
```
