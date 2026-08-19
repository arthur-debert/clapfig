# Config Command Guide

Clapfig provides a drop-in `config` subcommand for clap-based CLIs. Your users
get `config gen|list|get|set|unset|schema` with zero hand-written command logic.

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
    let builder = Clapfig::typed::<AppConfig>()
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

Generates a documented config template derived from the struct's `///` doc
comments and `#[clapfig(default)]` values, rendered in the app's **preferred
format** (the first enabled format — TOML unless the builder enables
others):

```sh
$ myapp config gen
# The host address to bind to.
host = "127.0.0.1"

# The port number.
port = 8080

# Enable debug mode.
debug = false

# Database settings.
[database]
# Connection string URL.
#url = ""

# Connection pool size.
pool_size = 10
```

Fields with defaults are real assignments; fields without one are commented
placeholders. Enum-typed fields additionally carry an `Allowed:` line;
array/map fields carry an `Elements:`/`Values:` line naming the element
type. A `Required.` line marks a placeholder the runtime rejects if left
commented (a non-optional scalar with no default). Absent arrays and maps
load as empty, so they do not get that line.

Write to a file with `--output` — the path's **extension selects the
format**, independent of the enabled-formats list:

```sh
myapp config gen --output myapp.toml
myapp config gen --output myapp.yaml   # YAML template
myapp config gen --output myapp.json   # JSON template
```

YAML templates use native comments, same shape as the TOML one. JSON has no
comment syntax, so JSON templates carry documentation as **`"//"` comment
keys** — the npm-blessed community convention:

```json
{
  "//host": "The host address to bind to.",
  "host": "127.0.0.1",
  "//port": "The port number.",
  "port": 8080,
  "//debug": "Enable debug mode.",
  "debug": false,
  "database": {
    "//": "Database settings.",
    "//url": [
      "Connection string URL.",
      "\"url\": \"\""
    ],
    "//pool_size": "Connection pool size.",
    "pool_size": 10
  }
}
```

The convention's rules: at most one bare `"//"` per object (the object's own
doc), suffixed `"//field-name"` keys for per-field docs, and an array of
strings for multi-line prose. Since JSON cannot comment out a real key,
defaultless fields show their assignment snippet *inside* the comment (the
`"\"url\": \"\""` line above). Comment keys are format syntax owned by the
JSON parser: every `//`-prefixed member is stripped at parse time, at any
nesting depth, so a generated template passes strict validation as-is. The
flip side is that the `//` key namespace is **reserved** — a `//`-prefixed
member in a JSON config file is always a comment, never a configuration
key. The exported JSON Schema (`config schema`) allowlists the `^//`
pattern so third-party validators accept documented templates too.

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
and the value is **parsed according to the leaf's declared type** before
writing — a string field takes `123` verbatim (no way needed to "force a
string"), a numeric or bool field refuses non-matching input naming the
expected type:

```sh
$ myapp config set port 9090
Set port = 9090

$ myapp config set port hello
# Error: Invalid value for 'port': expected integer, got 'hello'

$ myapp config set host 123        # host is a String field
Set host = 123                     # persists as the string "123"
```

Array and map leaves take TOML inline syntax — the value model's baseline
vocabulary, whatever format the target file uses; the parsed value is
written through that file's own adapter:

```sh
myapp config set tags '["a", "b"]'
myapp config set limits '{cpu = 2, mem = 8}'
```

Keys inside `ArrayOf`/`MapOf` sections (arrays or maps **of sections**,
e.g. `servers.web.host` where `servers` is a `HashMap<String, Server>`)
are not addressable with a dotted CLI key — the entry key is user data,
not a schema field, so `set` refuses with a targeted error telling you to
edit the config file directly. (An indexed path syntax is a possible
future extension.)

With `--scope`:

```sh
myapp config set port 9090 --scope global
```

Which file `set` writes depends on the scope's naming mode. An exact
`.file_name(...)` scope always targets that name. A `.file_stem(...)` scope
follows the preferred-format rules: exactly one same-stem file exists →
edit that file **in its own format**; none → create
`<stem>.<preferred extension>` seeded from the generated template; several
same-stem files → the same hard ambiguity error discovery raises. See
[Per-format editing](#per-format-editing) for what each format preserves.

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
let builder = Clapfig::typed::<AppConfig>()
    .app_name("myapp")
    .persist_scope("local", SearchPath::Cwd)       // default
    .persist_scope("global", SearchPath::Platform);
```

Scope paths are automatically added to the search path list, so persisted
values are always discoverable during `load()`.

## Per-format editing

`config set` and `config unset` edit files through each format's adapter,
and every format declares only what it can support **honestly** — asking for
more yields one typed "unsupported by this format" error instead of a lossy
rewrite:

- **TOML** — lossless editing via `toml_edit`: existing comments and
  formatting are fully preserved.
- **JSON** — comments are `"//"`-keyed data, so they survive edits for
  free. Formatting is normalized (pretty-printed, two-space indent);
  document key order is preserved, so comments stay adjacent to the fields
  they document.
- **YAML** — targeted span patching via `yamlpatch`: the edit rewrites only
  the target value's bytes and is byte-preserving (comments included)
  outside that span. Shapes the patch stack cannot rewrite honestly —
  replacing a sequence item, appending to a flow-style (`[a, b]`) list —
  **refuse with the typed error** rather than risking corruption:

  ```text
  replacing an existing value is unsupported by the yaml format
  ```

  Every YAML edit is verified after patching: the result must reparse to
  exactly the intended tree, so a refusal is always safe — the file is
  never left mangled.

If the target file doesn't exist, `config set` creates a new one seeded from
the generated template — so the user gets doc comments for every field out of
the box, in whichever format the scope resolves to. The template is rendered
with the builder's `normalize_keys` setting, so a seeded file spells its keys
the same way `config gen` does. With `normalize_keys(true)`, `set`, `unset`,
and `get` (merged and scoped alike) also accept the action key in either dash
or underscore spelling; edits land on the spelling already present in the
file. A file that already contains both equivalent spellings of a key —
anywhere in the file, even at a key the operation does not touch — is
ambiguous and fails with the same key-collision error loading it reports —
`set`, `unset`, and scoped `get` never operate on a file loading refuses.

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
    ConfigResult::KeyValue { key, value, doc, .. } => {
        // custom rendering
    }
    ConfigResult::Listing { entries, .. } => {
        for (key, value) in entries {
            // ...
        }
    }
    _ => println!("{result}"),
}
```

The `KeyValue`, `Listing`, and `ValueSet` variants also carry a
`rendered` field — the display block spelled in the **active format**
(the scope file's format for scoped operations, the preferred format for
merged views): `key = value` under TOML, `key: value` under YAML,
`"key": value` under JSON. `Display` (and therefore `handle_and_print` /
`handle_to_string`) prints that spelling, so `config get`/`list` output
matches the format your users actually write.

## ConfigCommand (runtime builder)

If your app already uses a `--scope` flag or has naming conflicts with
`ConfigArgs`, use `ConfigCommand` instead. It builds the clap command at
runtime and lets you rename subcommands and flags:

```rust
use clap::CommandFactory;
use clapfig::ConfigCommand;

let config_cmd = ConfigCommand::new()
    .scope_long("target")       // rename --scope to --target
    .gen_name("template");      // rename "gen" to "template"

let app = Cli::command()
    .subcommand(config_cmd.as_command("settings")); // "myapp settings" …

let matches = app.get_matches();
if let Some(("settings", sub)) = matches.subcommand() {
    let action = config_cmd.parse(sub)?;
    builder.handle_and_print(&action)?;
}
```

`as_command`'s `name` argument is the top-level subcommand (`"config"`,
`"settings"`, …). Per-item methods rename the nested subcommands and flags.
Both paths produce the same `ConfigAction`, so all downstream logic is shared.
Prefer `ConfigArgs` for simplicity; reach for `ConfigCommand` only when you
hit conflicts.
