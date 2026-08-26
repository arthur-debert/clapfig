---
name: clapfig-quick-howto
description: |
  Quick, copyable path for adopting Clapfig 0.23 in a Rust CLI project. Use when:
  (1) Adding layered configuration (defaults, config file, env vars, CLI flags)
      to a clap-based Rust app
  (2) Wiring the `config gen|list|get|set|unset|schema` command family
  (3) Giving users a persistent settings file (`myapp config set server.port 9090`)
      plus one-run CLI overrides (`myapp --port 9090 run`)
---

# Clapfig Quick How-To

Clapfig turns plain Rust structs into layered configuration: compiled
defaults, then config files, then env vars, then CLI flags — later layers
win. This is the minimal adoption path for Clapfig 0.23.

## 1. Install

```toml
[dependencies]
clapfig = "0.23"
```

The default features include `derive` (`#[derive(clapfig::Schema)]`) and
`clap` (the `config` subcommand integration).

## 2. Define the config structs — nested, in their owning modules

Each subsystem declares its own config struct **in the module that owns
it**; the application config composes them as fields. Do not grow one root
struct that spells out every subsystem's details.

```rust
// src/server.rs — the server module owns its config shape.
use clapfig::Schema;
use serde::{Deserialize, Serialize};

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct ServerConfig {
    /// Hostname to bind to.
    #[clapfig(default = "127.0.0.1")]
    pub host: String,

    /// Port number.
    #[clapfig(default = 8080)]
    pub port: u16,
}
```

```rust
// src/config.rs — the application config composes subsystem structs.
use clapfig::Schema;
use serde::{Deserialize, Serialize};

use crate::server::ServerConfig;

#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct AppConfig {
    /// Enable verbose output.
    #[clapfig(default = false)]
    pub verbose: bool,

    /// Server settings.
    pub server: ServerConfig,
}
```

What the derive gives you:

- `#[clapfig(default = ...)]` — the compiled default, the lowest layer.
- A nested `Schema` field becomes a config-file section (`[server]` in
  TOML), addressable as `server.port` dotted keys and `MYAPP__SERVER__PORT`
  env vars.
- `///` doc comments become the documentation in generated templates and
  `config get` output.
- Strict mode is on by default: an unknown key in a config file fails the
  load with the file, key, and line.

## 3. Wire clap: the `config` command family plus override flags

Embed `ConfigArgs` as one subcommand and add clap flags for the keys users
override per run:

```rust
// src/main.rs
use clap::{Parser, Subcommand};
use clapfig::{Clapfig, ConfigArgs, SearchPath, TypedBuilder};

use crate::config::AppConfig;

#[derive(Parser)]
#[command(name = "myapp")]
struct Cli {
    /// Override the server port for this run.
    #[arg(long, global = true)]
    port: Option<u16>,

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

fn make_builder(cli: &Cli) -> TypedBuilder<AppConfig> {
    Clapfig::typed::<AppConfig>()
        .app_name("myapp")
        // The user settings file: `config set` writes to the "user" scope,
        // which resolves to the platform config directory.
        .persist_scope("user", SearchPath::Platform)
        // One-run override: maps --port onto the nested key for this
        // process only; None means "flag not given, don't override".
        .cli_override("server.port", cli.port.map(i64::from))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let builder = make_builder(&cli);

    match cli.command {
        Commands::Run => {
            let config = builder.load()?;
            println!("listening on {}:{}", config.server.host, config.server.port);
        }
        Commands::Config(args) => {
            builder.handle_and_print(&args.into_action())?;
        }
    }
    Ok(())
}
```

For several top-level overrides, `cli_overrides_from(&overrides)` serializes
a struct and auto-matches fields to top-level config keys by name; nested
keys still need explicit `cli_override("section.key", ...)` calls.

## 4. The two override behaviors users get

**Persistent** — `config set` writes the value into the user settings file,
so it applies to every future run:

```sh
myapp config set server.port 9090   # persists to the "user" scope file
myapp run                           # runs on 9090 from now on
myapp config unset server.port      # back to the default
```

**One-run** — the clap flag overrides the key for this process only, on top
of whatever the file says; nothing is written:

```sh
myapp --port 9090 run               # this run only
```

## 5. The user settings file

`persist_scope("user", SearchPath::Platform)` names a persistence scope:

- **Where** — `SearchPath::Platform` is the OS config directory (XDG config
  home on Linux, `~/Library/...` on macOS, AppData on Windows), so the file
  lands where users expect app settings.
- **Writes** — `config set` / `config unset` edit the scope's file. If it
  does not exist yet, `set` creates `myapp.toml` there, seeded from the
  generated template so every field arrives with its doc comment.
- **Reads** — scope paths are automatically added to the search paths, so
  persisted values are always picked up by `load()`.
- The first scope added is the default; add more (e.g.
  `.persist_scope("local", SearchPath::Cwd)`) and users select one with
  `--scope`.

## 6. What the command family gives users

```sh
myapp config gen              # print a documented config template
myapp config list             # show all resolved values
myapp config get server.port  # one key, with its doc comment
myapp config set server.port 9090
myapp config unset server.port
myapp config schema           # JSON Schema for the config struct
```

For the full details, see the Clapfig repository docs:
`docs/getting-started.md`, `docs/config-command.md`,
`docs/derive-reference.md`, and `docs/layered-config.md`, plus the runnable
`crates/clapfig/examples/clapfig_demo` example.
