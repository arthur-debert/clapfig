# clapfig

Rich, layered configuration for Rust CLI apps. Define a struct, point at your files, and go.

**clapfig** orchestrates configuration from multiple sources — config files, environment variables, and CLI flags — through a builder API that takes a few lines to set up. Built on [confique](https://github.com/LukasKalbertodt/confique) for struct-driven defaults and commented template generation.

## Features

- **Struct as source of truth** — define settings as a Rust struct with defaults and `///` doc comments
- **Layered merge** — defaults < config files < env vars < CLI flags, every layer sparse
- **Multi-path file search** — platform config dir, home, cwd, or any path, in precedence order
- **Prefix-based env vars** — `MYAPP__DATABASE__URL` maps to `database.url` automatically
- **Clap override** — map individual clap args to config keys in one call each
- **Strict mode** — unknown keys in config files error with file path, key name, and line number (on by default)
- **Template generation** — `config gen` emits a commented sample config derived from the struct's doc comments
- **Config subcommand** — drop-in `config gen|get|set` commands for clap
- **Persistence** — `config set` patches values in place, preserving file comments

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

### Search Paths

```rust
use clapfig::{Clapfig, SearchPath};

let config: AppConfig = Clapfig::builder()
    .app_name("myapp")
    .search_paths(vec![
        SearchPath::Platform,                  // ~/.config/myapp/ on Linux
                                               // ~/Library/Application Support/myapp/ on macOS
        SearchPath::Home(".myapp"),             // ~/.myapp/
        SearchPath::Cwd,                       // ./
        SearchPath::Path("/etc/myapp".into()), // explicit absolute path
    ])
    .load()?;
```

Files load in order. **Later paths override earlier ones.** A `myapp.toml` in `./` overrides one in `~/.config/myapp/`, which overrides compiled-in defaults.

If a file doesn't exist at a given path, it's silently skipped.

### Strict Mode

Strict mode is **on by default**. If a config file contains a key that doesn't match any field in your struct, loading fails with a clear error including the file path, key name, and line number:

```
Unknown key 'typo_key' in /home/user/.config/myapp/myapp.toml (line 5)
```

Disable it with `.strict(false)` if you want to allow extra keys.

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

## Clap Integration

### CLI Overrides

#### Auto-matching

If your clap struct derives `Serialize`, `cli_overrides_from` auto-matches fields by name against config keys:

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
CLI overrides         .cli_override()
```

Every layer is **sparse**. You only specify the keys you want to override. Unset keys fall through to the next layer down.

## Persistence

`config set <key> <value>` writes to the primary config file (first resolved search path by default).

- If the file exists, the key is patched in place using [`toml_edit`](https://docs.rs/toml_edit), **preserving existing comments and formatting**.
- If the file doesn't exist, a fresh config is created from the generated template with the target key set.

## Full Example

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
