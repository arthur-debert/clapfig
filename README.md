# clapfig

Rich, layered configuration for Rust applications. Define a struct, point at your files, and go.

**clapfig** discovers, merges, and manages configuration from multiple sources — config files (TOML, YAML, or JSON), environment variables, and programmatic overrides — through a pure Rust builder API. The core library has **no dependency on any CLI framework**: you can use it in GUI apps, servers, or with any argument parser. For [clap](https://docs.rs/clap) users, an optional adapter provides drop-in `config gen|list|get|set|unset` subcommands with zero boilerplate.

Your struct is the schema: `#[derive(clapfig::Schema)]` captures types, defaults, enum sets, and doc comments, and every feature — loading, template generation, JSON Schema, persistence validation — reads from that one definition. Apps whose schema isn't known at compile time build the same schema at runtime instead.

## Features

**Core** (always available, no CLI framework needed):

- **Struct as source of truth** — define settings as a Rust struct with defaults and `///` doc comments; the derive emits the full schema (types, enum sets, docs) available at runtime
- **Layered merge** — defaults < config files < env vars < overrides, every layer sparse, [customizable precedence order](#layer-precedence)
- **Multi-format config files** — TOML, YAML, and JSON behind one format-adapter contract: `.file_stem("myapp")` plus an ordered opt-in formats list (TOML-only by default) discovers `myapp.toml` / `myapp.yaml` / `myapp.json`; identical validation, strictness, and error behavior in every format; per-format capabilities are declared, and unsupported operations refuse with a typed error instead of degrading silently
- **Multi-path file search** — platform config dir, home, cwd, ancestor walk, or any path
- **Search modes** — merge all found configs or use the first match
- **Ancestor walk** — walk up from cwd to find project configs, with configurable boundary (`.git`, filesystem root)
- **Tree-walk resolution** — build a reusable [`RuntimeResolver`](https://docs.rs/clapfig/latest/clapfig/struct.RuntimeResolver.html) once, call `.resolve_at(&dir)` for every leaf in a dynamic file tree (`.htaccess`/`.editorconfig` pattern). Per-call `Cwd`/`Ancestors` anchoring, instance-scoped file cache so repeated walks pay disk+parse once per unique file.
- **Runtime-defined schemas** — plugin hosts and generated apps build an owned [`Schema`](https://docs.rs/clapfig/latest/clapfig/runtime/struct.Schema.html) with a fluent builder and get the exact same pipeline; `load()` returns a value [`Map`](https://docs.rs/clapfig/latest/clapfig/value/type.Map.html)
- **Prefix-based env vars** — `MYAPP__DATABASE__URL` maps to `database.url` automatically
- **Kebab-case keys** — opt-in `.normalize_keys(true)` lets users write `pool-size = 5` in config files (or `--set database.pool-size=5` on the CLI) and have it map to a `pool_size` Rust field
- **Strict mode** — unknown keys error with file path, key name, and line number (on by default), with a cascading per-subtree override system and a per-key callback for the edge cases
- **Post-merge validation hook** — `.post_validate(|c| ...)` closes the gap between structural validation and the semantic constraints every real app has: port ranges, cross-field invariants, enum combinations, filesystem preconditions
- **Structured errors + rendering** — [`ClapfigError`](https://docs.rs/clapfig/latest/clapfig/error/enum.ClapfigError.html) carries data (keys, paths, lines, source text); the [`render`](https://docs.rs/clapfig/latest/clapfig/render/index.html) module turns it into plain text or [`miette`](https://docs.rs/miette)-style output with snippets and carets (rich mode behind the `rich-errors` feature)
- **Template generation** — emit a documented sample config from the struct's doc comments in any enabled format, including `Allowed:` lines for enum fields and typed placeholders for required fields; TOML and YAML use native comments, JSON carries docs via the community `"//"` comment-key convention
- **JSON Schema generation** — [`clapfig::schema::generate_schema`](https://docs.rs/clapfig/latest/clapfig/schema/fn.generate_schema.html) produces a Draft 2020-12 JSON Schema — with `type` on every field and `enum` sets — for UI editors, external validators, and IDE integrations; also exposed as `app config schema`
- **Persistence with named scopes** — global/local config file patterns with `--scope` targeting

**Clap adapter** (`clap` feature, on by default):

- **Config subcommand** — drop-in `config gen|get|set|unset|list` for clap
- **`--scope` flag** — target a specific scope for any config subcommand
- **Auto-matching overrides** — map clap args to config keys by name in one call

## Quick Start

```toml
[dependencies]
clapfig = "0.22"
```

Define your config with the `Schema` derive:

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

Load it:

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

Without clap:

```toml
clapfig = { version = "0.22", default-features = false, features = ["derive"] }
```

## Layer Precedence

```text
Compiled defaults     #[clapfig(default = ...)]
       ↑ overridden by
Config files          search paths in order, later paths win
       ↑ overridden by
Environment vars      MYAPP__KEY
       ↑ overridden by
URL query params      .url_query()          (requires "url" feature)
       ↑ overridden by
Overrides             .cli_override()
```

Every layer is **sparse**. You only specify the keys you want to override. Unset keys fall through to the next layer down.

This is the default order. You can customize it with `.layer_order()`:

```rust
use clapfig::{Clapfig, Layer};

let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
    .app_name("myapp")
    .layer_order(vec![Layer::Env, Layer::Files, Layer::Cli])
    .load()?;
```

Layers listed later override earlier ones. Omitting a layer excludes it from merging entirely. See [`Layer`](https://docs.rs/clapfig/latest/clapfig/enum.Layer.html) for the available variants.

## Config File Formats

Config files can be TOML, YAML, or JSON. Formats are **opt-in and ordered** — the default is TOML only, never inferred from cargo features:

```rust
let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
    .app_name("myapp")
    .file_stem("myapp")
    .formats(["toml", "yaml", "json"])
    .load()?;
```

`.file_stem("myapp")` discovers `myapp.toml`, `myapp.yaml`/`myapp.yml`, or `myapp.json` in every search directory; more than one same-stem match **in the same directory** is a hard error naming both files. The first enabled format is the app's **preferred format**: `config gen` renders it, and `config set` creates `<stem>.<preferred extension>` when no file exists yet. The exact-name `.file_name("myapp.toml")` alternative enables only that extension's format — existing TOML-only callers are unchanged.

Validation, strictness, and error behavior are identical in every format. Each format keeps its honest capabilities: TOML edits are lossless (`toml_edit`), JSON comments are `"//"`-keyed data and survive edits, and YAML edits are targeted span patches whose unsupported shapes refuse with a typed error rather than corrupting the file. See the [Config Command Guide](docs/config-command.md) for the per-format details.

## Demo

The repo includes a runnable example that exercises every feature:

```sh
cargo run --example clapfig_demo -- echo
cargo run --example clapfig_demo -- --color blue --port 8080 echo
cargo run --example clapfig_demo -- config gen
cargo run --example clapfig_demo -- config list
cargo run --example clapfig_demo -- config get server.port

# See the rich error renderer (miette) in action:
cargo run --example clapfig_demo --features rich-errors -- echo
# (drop a `clapfig-demo.toml` with an unknown key like `typo = 1` first)
```

See [`examples/clapfig_demo/`](crates/clapfig/examples/clapfig_demo/) for the full source.

## Tree-Walk Resolution

For tools that walk a file tree where every directory can have its own config — the `.editorconfig` / `.eslintrc` / `.htaccess` pattern — the resolver handle provides cached, per-directory resolution:

```rust
use clapfig::{Clapfig, SearchPath, Boundary};

let resolver = Clapfig::runtime(schema)
    .app_name("mytool")
    .file_name(".mytool.toml")
    .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
    .build_resolver()?;

// Each call resolves config independently from that directory,
// walking ancestors up to the .git boundary.
for dir in directories_to_process {
    let config = resolver.resolve_at(&dir)?;
    process(&dir, &config);
}
```

Key properties:

- **Per-call anchoring** — `SearchPath::Cwd` and `SearchPath::Ancestors` are relative to the directory passed to `resolve_at()`, not the process CWD.
- **File caching** — files are cached by absolute path inside the resolver. A tree walk over 1000 directories sharing 5 ancestor configs pays disk+parse once per unique file.
- **`post_validate` composition** — a validation hook registered on the builder fires on every `resolve_at()` call.

See the [RuntimeResolver docs](https://docs.rs/clapfig/latest/clapfig/struct.RuntimeResolver.html) for the full API.

## Documentation

**Guides** (in [`docs/`](docs/)):

- [Getting Started](docs/getting-started.md) — installation, first config struct, basic usage
- [Layered Configuration](docs/layered-config.md) — layers, search paths, merge modes, env vars, overrides
- [Runtime Schemas](docs/runtime-schemas.md) — building schemas at runtime for plugin hosts and generated apps
- [Strictness Guide](docs/strictness.md) — the cascading strict-mode system
- [Resolver Guide](docs/resolver.md) — per-directory resolution for tree-walk tools
- [Config Command Guide](docs/config-command.md) — the `config gen|list|get|set|unset` integration

**API reference**: the full API with design rationale lives in the [crate-level docs on docs.rs](https://docs.rs/clapfig).
