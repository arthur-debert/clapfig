# Config Command Guide

Clapfig provides a drop-in `config` subcommand for clap-based CLIs. Your users
get `config gen|list|get|set|unset` with zero hand-written command logic.

## Quick setup

Embed `ConfigArgs` in your clap subcommand enum:

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
    Run,
    Config(ConfigArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let builder = Clapfig::builder::<AppConfig>()
        .app_name("myapp")
        .persist_scope("local", SearchPath::Cwd)
        .persist_scope("global", SearchPath::Platform);

    match cli.command {
        Commands::Run => {
            let config = builder.load()?;
            // ...
        }
        Commands::Config(args) => {
            builder.handle_and_print(&args.into_action())?;
        }
    }
    Ok(())
}
```

## Available subcommands

### `config gen`

Generates a commented TOML template derived from the struct's `///` doc
comments and `#[config(default)]` values:

```sh
$ myapp config gen
## The host address to bind to.
#host = "127.0.0.1"

## The port number.
#port = 8080

[database]
## Connection string URL.
#url =

## Connection pool size.
#pool_size = 10
```

Write to a file with `--output`:

```sh
myapp config gen --output myapp.toml
```

### `config list`

Shows all resolved values from the merged config:

```sh
$ myapp config list
host = 127.0.0.1
port = 8080
debug = false
database.url = <not set>
database.pool_size = 10
```

With `--scope`, reads from a single scope's file (not the merged view):

```sh
$ myapp config list --scope local
port = 9090
```

### `config get <key>`

Shows a single key's value along with its doc comment:

```sh
$ myapp config get database.pool_size
# Connection pool size.
database.pool_size = 10
```

### `config set <key> <value>`

Persists a value to the config file. The key is validated against the struct
and the value is type-checked before writing:

```sh
$ myapp config set port 9090
Set port = 9090

$ myapp config set port hello
# Error: invalid type for key 'port'
```

With `--scope`:

```sh
$ myapp config set port 9090 --scope global
```

### `config unset <key>`

Removes a key from the config file:

```sh
$ myapp config unset port
Unset port
```

### `config schema`

Generates a JSON Schema (Draft 2020-12) describing the config struct:

```sh
myapp config schema
myapp config schema --output myapp-schema.json
```

## Persist scopes

Scopes name where `config set` and `config unset` write. The first scope
added to the builder is the default; users select others with `--scope`:

```rust
let builder = Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .persist_scope("local", SearchPath::Cwd)       // default
    .persist_scope("global", SearchPath::Platform);
```

Scope paths are automatically added to the search path list, so persisted
values are always discoverable during `load()`.

## Comment preservation

`config set` and `config unset` use `toml_edit` under the hood, so existing
comments and formatting in the config file are preserved. Users won't lose
their annotations when clapfig writes to the file.

If the target file doesn't exist, `config set` creates a new one seeded from
the generated template — so the user gets doc comments for every field out of
the box.

## Handling results programmatically

`handle_and_print()` prints to stdout, which is fine for most CLIs. If you
need the result as a string — for example, to feed it through a custom output
framework — use `handle_to_string()`:

```rust
let output = builder.handle_to_string(&action)?;
my_framework.write(&output);
```

Or use `handle()` directly for structured access to the result:

```rust
use clapfig::ConfigResult;

let result = builder.handle(&action)?;
match result {
    ConfigResult::KeyValue { key, value, doc } => {
        // custom rendering
    }
    ConfigResult::Listing { entries } => {
        for (key, value) in entries {
            // ...
        }
    }
    _ => println!("{result}"),
}
```

## ConfigCommand (runtime builder)

If your app already uses a `--scope` flag or has naming conflicts with
`ConfigArgs`, use `ConfigCommand` instead. It builds the clap command at
runtime and lets you rename subcommands and flags:

```rust
use clapfig::ConfigCommand;

let cmd = ConfigCommand::builder()
    .name("settings")          // "myapp settings" instead of "myapp config"
    .build();
```

Both paths produce the same `ConfigAction`, so all downstream logic is shared.
Prefer `ConfigArgs` for simplicity; reach for `ConfigCommand` only when you
hit conflicts.
