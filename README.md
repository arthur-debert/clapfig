# clapfig

Rich, layered configuration for Rust applications. Define a struct, point at your files, and go.

**clapfig** discovers, merges, and manages configuration from multiple sources — config files, environment variables, and programmatic overrides — through a pure Rust builder API. The core library has **no dependency on any CLI framework**: you can use it in GUI apps, servers, or with any argument parser. For [clap](https://docs.rs/clap) users, an optional adapter provides drop-in `config gen|list|get|set|unset` subcommands with zero boilerplate.

Built on [confique](https://github.com/LukasKalbertodt/confique) for struct-driven defaults and commented template generation.

## Features

**Core** (always available, no CLI framework needed):

- **Struct as source of truth** — define settings as a Rust struct with defaults and `///` doc comments
- **Layered merge** — defaults < config files < env vars < overrides, every layer sparse
- **Multi-path file search** — platform config dir, home, cwd, ancestor walk, or any path
- **Search modes** — merge all found configs or use the first match
- **Ancestor walk** — walk up from cwd to find project configs, with configurable boundary (`.git`, filesystem root)
- **Prefix-based env vars** — `MYAPP__DATABASE__URL` maps to `database.url` automatically
- **Strict mode** — unknown keys error with file path, key name, and line number (on by default)
- **Template generation** — emit a commented sample config from the struct's doc comments
- **Persistence with named scopes** — global/local config file patterns with `--scope` targeting

**Clap adapter** (`clap` feature, on by default):

- **Config subcommand** — drop-in `config gen|get|set|unset|list` for clap
- **`--scope` flag** — target a specific scope for any config subcommand
- **Auto-matching overrides** — map clap args to config keys by name in one call

## Quick Start

```toml
[dependencies]
clapfig = "0.10"
```

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

Load it:

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

Without clap:

```toml
clapfig = { version = "0.10", default-features = false }
```

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

## Demo

The repo includes a runnable example that exercises every feature:

```sh
cargo run --example clapfig_demo -- echo
cargo run --example clapfig_demo -- --color blue --port 8080 echo
cargo run --example clapfig_demo -- config gen
cargo run --example clapfig_demo -- config list
cargo run --example clapfig_demo -- config get server.port
```

See [`examples/clapfig_demo/`](examples/clapfig_demo/) for the full source.

## Documentation

The full guide — design rationale, search paths and modes, environment variables, programmatic overrides, persistence, clap adapter, template generation, strict mode, and normalizing values — lives in the [crate-level docs on docs.rs](https://docs.rs/clapfig).
