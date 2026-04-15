//! Rich, layered configuration for Rust applications. Define a struct, point
//! at your files, and go.
//!
//! Clapfig discovers, merges, and manages configuration from multiple sources
//! — config files, environment variables, and programmatic overrides — through
//! a builder API. Built on [confique](https://docs.rs/confique) for
//! struct-driven defaults and template generation.
//!
//! ```ignore
//! let config: AppConfig = Clapfig::builder()
//!     .app_name("myapp")
//!     .load()?;
//! ```
//!
//! That single call searches the platform config directory for `myapp.toml`,
//! merges `MYAPP__*` environment variables, fills in `#[config(default)]`
//! values, and hands you a typed struct.
//!
//! # Why clapfig
//!
//! Most applications need layered configuration: compiled defaults, a config
//! file, environment variables, maybe CLI flags. The typical approach is to
//! wire each source by hand — parse TOML, iterate env vars, map CLI args —
//! and the plumbing grows with every new source or setting.
//!
//! Clapfig replaces that plumbing with a single struct. The struct defines
//! which keys exist, what their defaults are, and what their documentation
//! says. Every operation — loading, template generation, `config get`,
//! `config set` — derives from that one definition. Add a field to the struct
//! and the config file, env vars, template, and CLI subcommands all pick it
//! up automatically.
//!
//! # Design: struct as source of truth
//!
//! Your config struct (via the `Config` derive, re-exported from confique) is
//! the schema for everything:
//!
//! - **`#[config(default = ...)]`** provides compiled defaults — the lowest
//!   layer, always present. Works with scalars (`default = 8080`), strings
//!   (`default = "localhost"`), and collections (`default = {}` for an empty
//!   map, `default = []` for an empty vec).
//! - **`///` doc comments** become the comments in generated templates and the
//!   output of `config get`.
//! - **`#[config(nested)]`** models hierarchical config. Nesting maps to TOML
//!   sections, dotted keys, and double-underscore env var separators.
//! - **`Option<T>` fields** are truly optional — omitting them in every source
//!   is valid. Fields without `Option` and without a default must be provided
//!   by at least one layer or loading fails.
//!
//! This means there is no separate schema file, no key registry, and no
//! chance of the template drifting from the code.
//!
//! # Core library — no CLI framework required
//!
//! The core of clapfig has **no dependency on any CLI framework**. Config
//! discovery, multi-file merging, environment variable mapping, key lookup,
//! persistence, and template generation all work through [`ClapfigBuilder`]
//! and [`ConfigAction`]. You can use clapfig in GUI apps, servers, embedded
//! tools, or with any argument parser.
//!
//! For [clap](https://docs.rs/clap) users, an optional adapter (the `cli`
//! module, behind the `clap` Cargo feature, on by default) provides drop-in
//! derive types that give your app `config gen|list|get|set|unset`
//! subcommands with zero boilerplate. To use clapfig without clap:
//!
//! ```toml
//! clapfig = { version = "...", default-features = false }
//! ```
//!
//! # Layer precedence
//!
//! ```text
//! Compiled defaults     #[config(default = ...)]
//!        ↑ overridden by
//! Config files          search paths in order, later paths win
//!        ↑ overridden by
//! Environment vars      PREFIX__KEY
//!        ↑ overridden by
//! URL query params      .url_query()          (requires "url" feature)
//!        ↑ overridden by
//! Overrides             .cli_override()
//! ```
//!
//! This is the default order. You can customize it with
//! [`layer_order()`](ClapfigBuilder::layer_order) — for example, to make
//! files override env vars, or to exclude a layer entirely:
//!
//! ```ignore
//! Clapfig::builder::<MyConfig>()
//!     .app_name("myapp")
//!     .layer_order(vec![Layer::Env, Layer::Files, Layer::Cli])
//!     .load()?;
//! ```
//!
//! See [`Layer`] for the available variants.
//!
//! Every layer is **sparse**. You only specify the keys you want to override
//! in that layer; unset keys fall through to the layer below. This is a
//! deliberate design choice: config files don't need to be complete, env vars
//! can target a single key, and CLI flags only override what the user
//! explicitly passes.
//!
//! # Three axes of file handling
//!
//! Config file behavior is controlled by three independent settings on the
//! builder. They compose freely — changing one doesn't affect the others.
//!
//! ## Discovery — where to look
//!
//! [`search_paths()`](ClapfigBuilder::search_paths) accepts a list of
//! [`SearchPath`] variants in **priority-ascending** order (last = highest):
//!
//! - **`Platform`** — the OS config directory (XDG on Linux, `~/Library/
//!   Application Support` on macOS). Good for user-level settings.
//! - **`Home(".myapp")`** — a dotfile directory under `$HOME`. Common for
//!   tools that predate XDG or target cross-platform consistency.
//! - **`Cwd`** — the working directory. Natural for project-local config.
//! - **`Path(path)`** — an explicit directory. Useful for system-wide
//!   defaults (`/etc/myapp/`) or test fixtures.
//! - **`Ancestors(boundary)`** — walks up from CWD, expanding into multiple
//!   directories (shallowest first, CWD last = highest priority). This is
//!   how tools like `.editorconfig` or `.eslintrc` work. The [`Boundary`]
//!   controls how far to walk: `Root` goes to the filesystem root;
//!   `Marker(".git")` stops at the repo boundary.
//!
//! Missing files are silently skipped — listing a search path is a
//! suggestion, not a requirement.
//!
//! ## Resolution — what to do with found files
//!
//! [`search_mode()`](ClapfigBuilder::search_mode) controls what happens
//! when multiple config files are found:
//!
//! - **[`Merge`](SearchMode::Merge)** (default) — deep-merge all files. Each
//!   file is a sparse overlay; later files override earlier ones key-by-key.
//!   Use this when configs are additive: a global file sets defaults, a
//!   project file overrides a few keys.
//!
//! - **[`FirstMatch`](SearchMode::FirstMatch)** — use only the single
//!   highest-priority file found. Use this when configs are self-contained
//!   and should not be layered: a code formatter whose project config
//!   replaces the user config entirely.
//!
//! The priority ordering is the same in both modes — switching between them
//! never requires reordering your search paths.
//!
//! ## Persistence — where to write
//!
//! [`persist_scope()`](ClapfigBuilder::persist_scope) names a target for
//! `config set`/`unset`. You can have multiple scopes (e.g. "local" and
//! "global") and the `--scope` flag selects which one to write to. The first
//! scope added is the default.
//!
//! Scope paths are automatically added to the search path list, so persisted
//! values are always discoverable during load. This means you don't need to
//! duplicate paths between `search_paths()` and `persist_scope()`.
//!
//! See the [`types`] module for common patterns: layered global + local,
//! fallback chains, nearest project config, per-directory layering.
//!
//! # Environment variables
//!
//! With env prefix `MYAPP`, variables map via double-underscore nesting:
//!
//! | Env var | Config key |
//! |---------|------------|
//! | `MYAPP__HOST` | `host` |
//! | `MYAPP__DATABASE__URL` | `database.url` |
//!
//! `__` (double underscore) separates nesting levels. Single `_` within a
//! segment is literal (part of the field name). Segments are lowercased to
//! match Rust field names.
//!
//! Values are parsed heuristically: `true`/`false` → bool, then integer,
//! then float, then string. This works well for the common case (ports,
//! flags, URLs). If you need exact control over how a value is interpreted,
//! use confique's `#[config(deserialize_with = ...)]` on the field.
//!
//! Disable env loading entirely with [`.no_env()`](ClapfigBuilder::no_env)
//! when you don't want environment variables in the mix (e.g. in tests or
//! embedded contexts).
//!
//! # URL query parameters
//!
//! *(Requires the `url` Cargo feature.)*
//!
//! For Rust-backed web applications, URL query parameters can serve as a
//! per-request config layer — by default sitting between environment variables
//! and CLI overrides in precedence (customizable via
//! [`layer_order()`](ClapfigBuilder::layer_order)). This is useful for WASM
//! frontends (Leptos, Dioxus, Yew) or server-side apps that accept config
//! overrides via the URL.
//!
//! ```ignore
//! let config: AppConfig = Clapfig::builder()
//!     .app_name("myapp")
//!     .url_query("port=9090&database.url=pg%3A%2F%2Fprod&debug=true")
//!     .load()?;
//! ```
//!
//! Keys use `.` for nesting (same as CLI overrides). Values are
//! percent-decoded and parsed with the same heuristic as env vars. A leading
//! `?` is stripped if present.
//!
//! | Query param | Config key | Parsed value |
//! |---|---|---|
//! | `port=9090` | `port` | `9090` (integer) |
//! | `database.url=pg%3A%2F%2Fprod` | `database.url` | `"pg://prod"` (string) |
//! | `debug=true` | `debug` | `true` (bool) |
//!
//! Clapfig is framework-agnostic — it takes a raw query string, not a
//! framework-specific request object. Your app extracts the query string
//! however it likes (`window.location.search`, request headers, etc.) and
//! passes it in.
//!
//! Enable the feature in your `Cargo.toml`:
//!
//! ```toml
//! clapfig = { version = "...", features = ["url"] }
//! ```
//!
//! # Programmatic overrides
//!
//! The [`cli_override()`](ClapfigBuilder::cli_override) and
//! [`cli_overrides_from()`](ClapfigBuilder::cli_overrides_from) methods
//! inject values at the `Cli` layer (highest priority by default). Despite
//! the name, they are not clap-specific — use them with any value source
//! (GUI inputs, HTTP headers, hardcoded test values). Their position in the
//! merge order can be changed with
//! [`layer_order()`](ClapfigBuilder::layer_order).
//!
//! `cli_overrides_from(source)` auto-matches: it serializes the source,
//! skips `None` values, and keeps only keys that exist in the config struct.
//! This means you can pass your entire clap struct and non-config fields
//! (`command`, `verbose`, `output`) are silently ignored. For fields where
//! the CLI name differs from the config key (e.g. `--db-url` vs
//! `database.url`), use `cli_override("database.url", cli.db_url)`.
//!
//! Both methods push to the same override list and compose freely. Later
//! calls take precedence.
//!
//! # Strict mode
//!
//! Strict mode is **on by default**. When a config file contains a key that
//! doesn't match any field in your struct, loading fails with the file path,
//! key name, and line number:
//!
//! ```text
//! Unknown key 'typo_key' in /home/user/.config/myapp/myapp.toml (line 5)
//! ```
//!
//! This catches typos and stale keys early. Turn it off with
//! [`.strict(false)`](ClapfigBuilder::strict) if you intentionally share
//! config files across tools or want forward-compatible configs.
//!
//! # Semantic validation — the `post_validate` hook
//!
//! Strict mode and confique together cover **structural** validation: every
//! key is known, every required field is present, every value has the right
//! type. They do not cover **semantic** constraints — the rules that depend
//! on the merged value rather than a single field's type:
//!
//! - numeric ranges (`port >= 1024`, `quality <= 100`, `pool_size > 0`)
//! - cross-field invariants (`if tls_enabled then tls_cert_path must be set`)
//! - enum combinations (`mode == "fast" requires buffer_size < 64k`)
//! - filesystem preconditions (`output_dir must exist and be writable`)
//! - anything that needs the final, fully-merged `&C` to decide
//!
//! Write them once, in a closure, and register it on the builder:
//!
//! ```ignore
//! let config: AppConfig = Clapfig::builder()
//!     .app_name("myapp")
//!     .post_validate(|c| {
//!         if c.port < 1024 {
//!             return Err(format!("port {} is below 1024", c.port));
//!         }
//!         if c.tls_enabled && c.tls_cert_path.is_none() {
//!             return Err("tls_enabled requires tls_cert_path".into());
//!         }
//!         Ok(())
//!     })
//!     .load()?;
//! ```
//!
//! The hook runs after all layers have been merged and confique has
//! type-validated the result, but before [`load()`](ClapfigBuilder::load)
//! returns. Rejections become
//! [`ClapfigError::PostValidationFailed`],
//! which renders with the same error pipeline as every other clapfig error.
//!
//! Design notes:
//!
//! - **Signature is `Fn(&C) -> Result<(), String>`.** String keeps the API
//!   tiny. Callers with richer error types call `.map_err(|e| e.to_string())`.
//! - **Upstream failures short-circuit.** Parse errors, strict-mode
//!   violations, and type errors all fire before the hook, so the hook only
//!   ever sees a fully-valid `&C`.
//! - **One hook per builder.** Calling `.post_validate()` twice replaces the
//!   previous hook — compose multiple checks inside one closure.
//! - **The hook is captured by value and fires on every resolution.** If you
//!   build a [`Resolver<C>`](Resolver) for tree-walk use cases (see the next
//!   section), the same hook runs on every [`resolve_at()`](Resolver::resolve_at)
//!   call.
//!
//! # Tree-walk resolution — the `Resolver<C>` handle
//!
//! [`load()`](ClapfigBuilder::load) assumes one resolution per process,
//! anchored at `std::env::current_dir()`. For one-shot CLI tools that's
//! exactly right. But for tools that walk a **dynamic file tree** where every
//! directory can have an optional config — the `.htaccess` / `.gitignore` /
//! `.editorconfig` / `.eslintrc` pattern — you need N resolutions from N
//! different anchors, and you want to amortize the I/O cost across calls.
//!
//! [`ClapfigBuilder::build_resolver()`] gives you a reusable
//! [`Resolver<C>`](Resolver) handle for that case:
//!
//! ```ignore
//! let resolver = Clapfig::builder::<MyConfig>()
//!     .app_name("myapp")
//!     .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
//!     .post_validate(|c| validate_ranges(c))
//!     .build_resolver()?;
//!
//! for leaf in walk_content_tree("./site") {
//!     let cfg = resolver.resolve_at(&leaf)?;
//!     render_page(&leaf, &cfg);
//! }
//! ```
//!
//! Key properties:
//!
//! - **`SearchPath::Cwd` and `SearchPath::Ancestors` are anchored at the
//!   directory passed to [`resolve_at()`](Resolver::resolve_at)**, not at the
//!   process CWD. Each call is a fully independent resolution with the same
//!   builder state but a different starting point. Walking a site tree with
//!   `Ancestors(Boundary::Marker(".git"))` gives you nearest-project-config
//!   semantics on every leaf for free.
//! - **Files are cached by absolute path inside the resolver.** A tree walk
//!   that visits 1000 leaves sharing 5 ancestor config files pays the disk +
//!   parse cost once per unique file, not 1000×. The `Resolver` is the cache
//!   scope — drop it to invalidate everything.
//! - **No mtime checking.** The cache is not invalidated when files change on
//!   disk. Long-lived processes that need freshness should build a new
//!   `Resolver`. This is a deliberate "keep v1 simple" choice; the contract
//!   is documented and a regression test locks it in.
//! - **`load()` is the special case.** Internally `load()` is just
//!   `self.build_resolver()?.resolve_at(std::env::current_dir()?)`, so
//!   existing single-shot callers get zero behavior change and all resolution
//!   flows through one code path.
//! - **The `post_validate` hook composes naturally.** Registered once on the
//!   builder, captured into the resolver, fired on every `resolve_at` call —
//!   so per-leaf invariants get the same enforcement as top-level `load()`.
//!
//! See [`Resolver`] for the full API.
//!
//! # Normalizing values
//!
//! Use confique's `#[config(deserialize_with = ...)]` to normalize values
//! during deserialization. The function runs automatically when a value is
//! loaded from any source — config files, env vars, or overrides. This is
//! useful for case-insensitive fields, path canonicalization, or unit
//! conversion.
//!
//! Note that `#[config(default)]` values are injected directly by confique
//! and **do not** pass through the deserializer — write defaults in their
//! already-normalized form.
//!
//! # Template generation
//!
//! `config gen` (or [`ConfigAction::Gen`]) produces a commented TOML file
//! derived from the struct's `///` doc comments and `#[config(default)]`
//! values. The template stays in sync with code — change a doc comment or a
//! default, the template reflects it. When `config set` creates a new file,
//! it seeds it from this template so the user gets a documented starting
//! point.
//!
//! # Clap adapter
//!
//! The `cli` module (behind the `clap` feature) offers two integration
//! paths:
//!
//! - **[`ConfigArgs`]** — a clap derive struct you embed in your
//!   `#[derive(Subcommand)]` enum. Fastest path: one line to add, one call
//!   to [`into_action()`](ConfigArgs::into_action) to bridge to the core.
//!
//! - **[`ConfigCommand`]** — a runtime builder for apps that need to rename
//!   subcommands or flags (e.g. if your app already has a `--scope` flag).
//!   Produces the same [`ConfigAction`], so all downstream logic is shared.
//!
//! Both paths give your users `config gen|list|get|set|unset` with `--scope`
//! support. Pick `ConfigArgs` for simplicity; reach for `ConfigCommand` only
//! when you hit naming conflicts.
//!
//! # Persistence
//!
//! `config set` and `config unset` write to config files through named
//! persist scopes. Key design decisions:
//!
//! - **Comment preservation**: edits use `toml_edit`, so existing comments
//!   and formatting are preserved. Users won't lose their annotations.
//! - **Seeded files**: if the target file doesn't exist, a new one is created
//!   from the generated template, so the user gets doc comments for every
//!   field out of the box.
//! - **Validation before write**: `config set` validates that the key exists
//!   and the value is type-compatible before touching the file. A typo in the
//!   key name or a string where an integer is expected fails fast.
//! - **Scoped reads**: `config list --scope global` and `config get key
//!   --scope local` read from a single scope's file rather than the merged
//!   view, letting users inspect where values come from.
//!
//! # Error handling
//!
//! All fallible operations return [`ClapfigError`]. Errors are designed to
//! be user-facing: unknown keys include file paths and line numbers, unknown
//! scopes list the available ones, and missing prerequisites reference the
//! builder method to call. See the [`error`] module for the full set.

pub mod error;
pub mod render;
pub mod schema;
pub mod types;

mod builder;
#[cfg(feature = "clap")]
mod cli;
mod env;
mod file;
mod flatten;
pub(crate) mod merge;
mod ops;
mod overrides;
mod persist;
mod resolve;
mod resolver;
#[cfg(feature = "url")]
mod url;
mod validate;

#[cfg(test)]
mod fixtures;

pub use builder::{Clapfig, ClapfigBuilder};
#[cfg(feature = "clap")]
pub use cli::{ConfigArgs, ConfigCommand, ConfigSubcommand};
pub use confique::Config;
pub use error::{ClapfigError, UnknownKeyInfo};
pub use ops::ConfigResult;
pub use resolver::Resolver;
pub use types::{Boundary, ConfigAction, Layer, SearchMode, SearchPath};
