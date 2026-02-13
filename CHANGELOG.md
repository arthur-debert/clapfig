# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- **Fixed**
  - **`config set` now validates values before persisting** - Previously, `config set mode garbage` would silently write invalid values to the TOML file, only surfacing errors on the next `load()`. Now `set_in_document` validates both that the key exists in the config schema and that the value is type-compatible (e.g. valid enum variant) by round-trip deserializing into `C::Layer` before writing. Invalid values produce a clear `InvalidValue` error; unknown keys produce `KeyNotFound`. This closes the structural gap where `load()` and `cli_overrides_from()` validated but `config set` did not. ([#9](https://github.com/arthur-debert/clapfig/issues/9))
- **Added**
  - **Named persist scopes** - New `.persist_scope(name, path)` builder method replaces `.persist_path()`. Scopes are named config file targets (e.g. "local", "global") for read/write operations. The first scope added is the default for writes. Scope paths are automatically added to search paths for discovery.
  - **`--scope` CLI flag** - New `--scope <name>` global flag on `ConfigArgs` targets a specific persist scope. Works with all config subcommands: `set`/`unset` write to the named scope; `list`/`get` read from that scope's file only (instead of the merged resolved view).
  - **Scope file operations** - `list_scope_file()` and `get_scope_value()` read entries from individual scope files without going through the full resolve pipeline.
  - **`UnknownScope` error** - New `ClapfigError::UnknownScope` with the invalid scope name and list of available scopes.
  - **`config list`** - New `ConfigAction::List` and `ConfigSubcommand::List` to show all resolved key-value pairs. Bare `app config` (no subcommand) defaults to list. Uses the flatten module to display dotted keys, with `<not set>` for unset optional fields.
  - **Demo application** - `examples/clapfig_demo/` is a runnable sample CLI that exercises all core features: multi-path file search, env var overrides (`CLAPFIG_DEMO__*`), CLI flag mapping (both `cli_overrides_from` and manual `cli_override`), nested config structs, `config gen|get|set|list` subcommands, and ANSI-colored terminal output. Run with `cargo run --example clapfig_demo -- echo`.
  - **Search modes** - New `SearchMode` enum on the builder via `.search_mode()`. `Merge` (default) deep-merges all found config files; `FirstMatch` uses only the highest-priority file found ("find my config" pattern).
  - **Ancestor walk** - New `SearchPath::Ancestors(Boundary)` variant walks up from the current working directory to discover config files. `Boundary::Root` walks to the filesystem root; `Boundary::Marker(".git")` stops at a project boundary (inclusive). Expands inline in the search path list, composable with other variants.
  - **Feature-gated clap dependency** - `clap` is now an optional dependency behind the `clap` Cargo feature (enabled by default). The `cli` module, `ConfigArgs`, and `ConfigSubcommand` are only compiled when the feature is active. Use `default-features = false` to use clapfig without pulling in clap.
- **Changed**
  - `.persist_path()` replaced by `.persist_scope(name, path)`. Scopes are named targets; the first added is the default.
  - `ConfigAction` variants `List`, `Get`, `Set`, `Unset` now carry `scope: Option<String>`.
  - `config set`/`unset` require at least one `.persist_scope()`. Omitting returns `ClapfigError::NoPersistPath`.
  - `load_config_files` now takes a `SearchMode` parameter.
  - New public exports: `Boundary`, `SearchMode`.
  - Crate and README documentation restructured to clarify that the core API is framework-agnostic and clap is an optional adapter.

## [0.2.0] - 2026-02-12

- **Added**
  - **Auto-match CLI overrides** - `.cli_overrides_from(&cli_struct)` auto-matches serializable struct fields to config keys by name, skipping `None` values and ignoring non-matching fields. Works with clap structs, `HashMap`s, or any `Serialize` type. Composes with manual `.cli_override()`.
  - **`Display` for `ConfigResult`** - `ConfigResult` now implements `Display`, returning the formatted output as a string
  - **`handle_and_print`** - Convenience method on `ClapfigBuilder` that calls `handle()` and prints the result

## [0.1.0] - 2026-02-11

- **Added**
  - **Struct-driven config** - Define settings as a Rust struct with `#[config(default)]` and doc comments via confique
  - **Layered merge** - defaults < config files < env vars < CLI flags, every layer sparse
  - **Multi-path file search** - Platform config dir, home subdir, cwd, or explicit paths
  - **Prefix-based env vars** - `MYAPP__DATABASE__URL` maps to `database.url` with heuristic type parsing
  - **Clap CLI overrides** - `.cli_override("key", value)` maps any clap arg to any config key
  - **Strict mode** - Unknown keys in config files error with file path, key name, and line number (on by default)
  - **Template generation** - `config gen` emits a commented TOML template from struct doc comments
  - **Config subcommand** - Drop-in `config gen|get|set` commands for clap
  - **Persistence** - `config set` patches values in place via `toml_edit`, preserving comments and formatting

[0.2.0]: https://github.com/arthur-debert/clapfig/releases/tag/v0.2.0
[0.1.0]: https://github.com/arthur-debert/clapfig/releases/tag/v0.1.0
