# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- **Added**
  - **`config list`** - New `ConfigAction::List` and `ConfigSubcommand::List` to show all resolved key-value pairs. Bare `app config` (no subcommand) defaults to list. Uses the flatten module to display dotted keys, with `<not set>` for unset optional fields.

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
