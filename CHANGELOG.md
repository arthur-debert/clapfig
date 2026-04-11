# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- **Added**
  - **`Resolver<C>` and tree-walk resolution** — New `ClapfigBuilder::build_resolver()` method returns a reusable `Resolver<C>` handle that can be called repeatedly with `.resolve_at(&dir)` to produce a typed configuration anchored at a specific directory. Unlocks the `.htaccess` / `.gitignore` / `.editorconfig` pattern where every directory in a dynamic file tree is its own resolution root. `SearchPath::Cwd` and `SearchPath::Ancestors` are interpreted relative to the directory passed to each `resolve_at` call instead of the process's current working directory, so walking a content tree yields one independently-merged config per leaf. Files read during resolution are cached inside the resolver by absolute path, so tools that walk a 1000-leaf tree sharing five ancestor config files pay the disk+parse cost once per unique file instead of 1000×. Any `.post_validate()` hook registered on the builder is captured into the resolver and fires on every `resolve_at` call. New public export: `Resolver`.
  - **`.post_validate()` hook** — New builder method `ClapfigBuilder::post_validate(|c: &C| -> Result<(), String>)` registers a closure that runs after all layers have been merged and confique has type-validated the result, but before `load()` returns. Use it for constraints confique can't express: numeric ranges ("port must be ≥ 1024"), cross-field invariants ("if A is set then B must be set"), enum combinations, filesystem preconditions, or anything that depends on the merged value. Rejection messages are wrapped in the new `ClapfigError::PostValidationFailed(String)` variant. The hook runs only when upstream resolution (parsing, strict-mode validation, type-checking) succeeds; failures short-circuit before it fires. Calling `.post_validate()` more than once replaces the previous hook.
  - **JSON Schema generation** — New `clapfig::schema::generate_schema::<C>()` walks confique's `Meta` tree to produce a JSON Schema (Draft 2020-12) document describing the config struct. Each nested struct becomes an `object` with `properties`; non-`Option<T>` fields are listed in `required`. Leaf types are inferred from `#[config(default = ...)]` expressions (string, integer, number, boolean, array, object), doc comments become `description`, defaults are emitted as `default`, and env var names are attached as the `x-env` extension. Intended for auto-generating UI editors, external validation tools, and IDE integrations. Exposed via `app config schema` (with `-o/--output` to write to a file) on both `ConfigArgs` (derive) and `ConfigCommand` (runtime builder, renameable via `.schema_name()`). New `ConfigAction::Schema`, `ConfigSubcommand::Schema`, `ConfigResult::Schema` / `SchemaWritten` variants. Fields without defaults are emitted without a `type` key (any JSON value accepted) since confique's `Meta` does not carry Rust type information directly.
  - **Structured error data API** — New `UnknownKeyInfo` struct (flat `{ key, path, line, source }`) replaces the nested `ClapfigError::UnknownKey`/`UnknownKeys(Vec<ClapfigError>)` shape. Accessor methods `ClapfigError::unknown_keys() -> Option<&[UnknownKeyInfo]>`, `parse_error() -> Option<(&Path, &toml::de::Error, Option<&str>)>`, and `is_strict_violation() -> bool` let callers read error data without pattern-matching on enum variants. Full source text is retained (as `Arc<str>`) on unknown-key and parse errors so renderers can draw snippets. New public export: `UnknownKeyInfo`.
  - **`clapfig::render` module** — Presentation layer separate from `ClapfigError`. `render_plain(&err) -> String` produces ANSI-free output with source snippets and carets, always available. `render_rich(&err) -> String` produces colored, aligned output via [`miette`](https://docs.rs/miette)'s graphical report handler, behind the new `rich-errors` Cargo feature. Both functions return strings — the caller decides where output lands (stderr, log file, TUI pane).
  - **`rich-errors` Cargo feature** — Opt-in `miette` integration for terminal-quality error output. Disabled by default to keep the dependency footprint small.
  - **Configurable layer precedence** — New `Layer` enum and `.layer_order()` builder method allow customizing the merge order of configuration sources. The default order (`Files < Env < Url < Cli`) is preserved when unset. Layers listed later override earlier ones; omitting a layer excludes it from merging entirely. New public export: `Layer`.
  - **URL query parameter layer** — New `.url_query()` builder method parses URL query strings (`port=9090&database.url=pg%3A%2F%2Fprod`) into config overrides. Keys use `.` for nesting, values are percent-decoded and parsed with the same heuristic as env vars. Sits between env vars and CLI overrides in precedence: defaults < files < env < **URL** < CLI. Requires the `url` Cargo feature (`dep:percent-encoding`).
- **Documentation**
  - **Restructured documentation** — Moved the comprehensive user guide from README into crate-level doc comments (published to docs.rs), covering design rationale, trade-offs, and "when to use what" guidance. Added module-level docs to `builder` and `error` modules. Slimmed README from 537 to 126 lines as a landing page with feature list, quick start, and link to docs.rs. ([#13](https://github.com/arthur-debert/clapfig/pull/13))
- **Fixed**
  - **`config set` now validates values before persisting** - Previously, `config set mode garbage` would silently write invalid values to the TOML file, only surfacing errors on the next `load()`. Now `set_in_document` validates both that the key exists in the config schema and that the value is type-compatible (e.g. valid enum variant) by round-trip deserializing into `C::Layer` before writing. Invalid values produce a clear `InvalidValue` error; unknown keys produce `KeyNotFound`. This closes the structural gap where `load()` and `cli_overrides_from()` validated but `config set` did not. ([#9](https://github.com/arthur-debert/clapfig/issues/9))
- **Added**
  - **`ConfigCommand` builder** — Runtime-configurable alternative to `ConfigArgs` for apps that need to rename config subcommands or flags to avoid conflicts with their own CLI. Builder methods: `list_name()`, `gen_name()`, `get_name()`, `set_name()`, `unset_name()`, `scope_long()`, `output_long()`, `output_short()`. Produces the same `ConfigAction` as the derive path, so all downstream logic is shared. ([#11](https://github.com/arthur-debert/clapfig/issues/11))
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
  - **`ClapfigError` error variants restructured** — The single-key `ClapfigError::UnknownKey { key, path, line }` variant was removed; all strict-mode violations now flow through `ClapfigError::UnknownKeys(Vec<UnknownKeyInfo>)` (a one-element vector for the single-key case). `ClapfigError::ParseError` now wraps `Box<toml::de::Error>` (to keep the enum variant small) and carries a new `source_text: Option<Arc<str>>` field with the retained file contents for snippet rendering. Callers pattern-matching on these variants will need to update; the new accessor methods (`unknown_keys()`, `parse_error()`) are a more stable alternative.
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
