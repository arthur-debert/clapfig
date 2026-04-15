# Layered Configuration

Clapfig merges configuration from multiple sources — compiled defaults, config
files, environment variables, and programmatic overrides — into a single typed
struct. This guide covers how layers work, how to control file discovery and
merge behavior, and common patterns.

## Layer precedence

By default, layers are merged in this order (later wins):

```
Compiled defaults     #[config(default = ...)]
       ↑ overridden by
Config files          search paths in order, later paths win
       ↑ overridden by
Environment vars      PREFIX__KEY
       ↑ overridden by
URL query params      .url_query()          (requires "url" feature)
       ↑ overridden by
Overrides             .cli_override()
```

Every layer is **sparse** — you only specify the keys you want to override.
Unset keys fall through to the layer below.

### Customizing layer order

```rust
use clapfig::{Clapfig, Layer};

let config: AppConfig = Clapfig::builder()
    .app_name("myapp")
    .layer_order(vec![Layer::Env, Layer::Files, Layer::Cli])
    .load()?;
```

Layers listed later override earlier ones. Omitting a layer excludes it
entirely — the example above makes files override env vars (reversed from the
default).

## File discovery — search paths

`search_paths()` accepts a list of `SearchPath` variants in priority-ascending
order (last = highest priority):

| Variant | Resolves to | Use case |
|---------|-------------|----------|
| `Platform` | OS config dir (XDG, `~/Library/...`, AppData) | User-level settings |
| `Home(".myapp")` | `$HOME/.myapp/` | Cross-platform dotfile convention |
| `Cwd` | Working directory | Project-local config |
| `Path(path)` | Explicit directory | System defaults (`/etc/myapp/`), test fixtures |
| `Ancestors(boundary)` | Walk up from CWD | `.editorconfig`-style per-directory config |

Missing files are silently skipped — listing a search path is a suggestion,
not a requirement.

### Ancestor walks

`SearchPath::Ancestors(boundary)` walks up the directory tree from the working
directory, expanding into multiple directories (shallowest first, CWD last =
highest priority). The `Boundary` controls how far:

- **`Boundary::Root`** — walk to the filesystem root.
- **`Boundary::Marker(".git")`** — stop at the first directory containing
  `.git`. The marker directory **is** included in the search.

```rust
use clapfig::{SearchPath, Boundary};

// Find configs anywhere between CWD and the repo root
SearchPath::Ancestors(Boundary::Marker(".git"))

// Walk all the way up
SearchPath::Ancestors(Boundary::Root)
```

## File resolution — search modes

`search_mode()` controls what happens when multiple config files are found:

### `SearchMode::Merge` (default)

Deep-merges all files. Each file is a sparse overlay — later files override
earlier ones key by key. Use this when configs are additive:

```
~/.config/myapp/myapp.toml    → host = "0.0.0.0", port = 8080
./myapp.toml                  → port = 3000
```

Result: `host = "0.0.0.0"` (from platform), `port = 3000` (from cwd).

### `SearchMode::FirstMatch`

Uses only the single highest-priority file found. Use this when configs are
self-contained and should not layer — a formatter whose project config
replaces the user config entirely.

The priority ordering is the same in both modes. Switching between them never
requires reordering your search paths.

## Environment variables

With `env_prefix("MYAPP")` (or derived from `app_name("myapp")`):

| Env var | Config key |
|---------|------------|
| `MYAPP__HOST` | `host` |
| `MYAPP__DATABASE__URL` | `database.url` |

`__` (double underscore) separates nesting levels. Single `_` within a segment
is literal. Segments are lowercased to match Rust field names.

Values are parsed heuristically: `true`/`false` → bool, then integer, then
float, then string. For exact control, use confique's
`#[config(deserialize_with = ...)]`.

Disable env entirely with `.no_env()`.

## Programmatic overrides

`.cli_override()` and `.cli_overrides_from()` inject values at the `Cli`
layer (highest priority by default):

```rust
// Manual mapping for nested keys
builder.cli_override("database.url", cli.db_url.clone())

// Auto-match: serializes the struct, keeps only matching keys
builder.cli_overrides_from(&cli_struct)
```

`cli_overrides_from` is useful with clap: pass your entire CLI args struct and
non-config fields are silently ignored.

## Persistence — where writes go

`persist_scope()` names a target for `config set` and `config unset`:

```rust
let builder = Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .persist_scope("local", SearchPath::Cwd)
    .persist_scope("global", SearchPath::Platform);
```

The first scope is the default. Users select others with `--scope`:

```sh
myapp config set port 9090 --scope global
```

Scope paths are automatically added to the search path list, so persisted
values are always discoverable during `load()`.

## Post-merge validation

Structural validation (known keys, required fields, correct types) is handled
by confique and strict mode. For semantic constraints that need the final
merged config, use `post_validate`:

```rust
let config: AppConfig = Clapfig::builder()
    .app_name("myapp")
    .post_validate(|c| {
        if c.port < 1024 {
            return Err(format!("port {} is below 1024", c.port));
        }
        if c.tls_enabled && c.tls_cert_path.is_none() {
            return Err("tls_enabled requires tls_cert_path".into());
        }
        Ok(())
    })
    .load()?;
```

The hook runs after all layers have been merged and type-validated. Rejections
become `ClapfigError::PostValidationFailed`.

## Common patterns

### Global + local config

```rust
Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .search_paths(vec![
        SearchPath::Platform,   // ~/.config/myapp/myapp.toml (global)
        SearchPath::Cwd,        // ./myapp.toml (local, wins)
    ])
    .persist_scope("local", SearchPath::Cwd)
    .persist_scope("global", SearchPath::Platform)
    .load()?;
```

### No env vars, no files — just defaults + overrides

```rust
Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .no_env()
    .search_paths(vec![])
    .cli_override("port", 9090)
    .load()?;
```

### Per-project config with repo boundary

```rust
Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .search_paths(vec![
        SearchPath::Platform,
        SearchPath::Ancestors(Boundary::Marker(".git")),
    ])
    .load()?;
```
