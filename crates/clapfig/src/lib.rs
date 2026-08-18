//! Rich, layered configuration for Rust applications. Define a struct, point
//! at your files, and go.
//!
//! Clapfig discovers, merges, and manages configuration from multiple sources
//! — config files, environment variables, and programmatic overrides — through
//! a builder API. The schema comes from your own struct via
//! `#[derive(clapfig::Schema)]`, or from a runtime-built
//! [`Schema`](runtime::Schema) for apps whose schema isn't known at compile
//! time.
//!
//! ```ignore
//! use clapfig::{Clapfig, Schema};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Schema, Serialize, Deserialize, Debug)]
//! struct AppConfig {
//!     /// Listen host.
//!     #[clapfig(default = "localhost")]
//!     host: String,
//!
//!     /// Listen port.
//!     #[clapfig(default = 8080)]
//!     port: u16,
//! }
//!
//! let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
//!     .app_name("myapp")
//!     .load()?;
//! ```
//!
//! That single call searches the platform config directory for `myapp.toml`,
//! merges `MYAPP__*` environment variables, fills in `#[clapfig(default)]`
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
//! Your config struct (via `#[derive(clapfig::Schema)]`) is the schema for
//! everything:
//!
//! - **`#[clapfig(default = ...)]`** provides compiled defaults — the lowest
//!   layer, always present. Works with scalars (`default = 8080`), strings
//!   (`default = "localhost"`), and collections (`default = []` for an
//!   empty vec).
//! - **`///` doc comments** become the comments in generated templates and the
//!   output of `config get`.
//! - **Nested structs** (fields whose type also derives `Schema`) model
//!   hierarchical config. Nesting maps to config-file sections (TOML
//!   `[section]`, YAML/JSON nesting), dotted keys, and double-underscore
//!   env var separators.
//! - **Unit-only enums** deriving `Schema` become constrained value sets:
//!   out-of-set values error at load, templates document the allowed set
//!   with an `Allowed: ...` annotation (native comments in TOML/YAML,
//!   `"//"` comment keys in JSON), and the JSON Schema emits `enum: [...]`.
//! - **`Option<T>` fields** are truly optional — omitting them in every source
//!   is valid. Fields without `Option` and without a default must be provided
//!   by at least one layer or loading fails.
//!
//! This means there is no separate schema file, no key registry, and no
//! chance of the template drifting from the code. The full schema — types,
//! enum sets, docs, defaults — is available at runtime, so JSON Schema
//! generation, template generation, and persistence validation all see the
//! same metadata regardless of which entry point built the schema.
//!
//! # Design: clapfig owns its value model
//!
//! Clapfig is a configuration tool, not a TOML tool. The whole pipeline —
//! layer construction, merging, validation, defaults, resolution output —
//! traffics in clapfig's own [`value::Value`] (string, integer, float,
//! bool, datetime, array, unordered map), never in a serialization
//! crate's types. Serialization formats get the same treatment as CLI
//! frameworks: **adapters at the boundary** (the [`format`](mod@format)
//! module), one
//! per format — TOML, YAML, JSON — behind a single contract
//! ([`format::FormatAdapter`]) that parses text into `Value`, renders
//! documented templates, serializes, and edits files.
//!
//! The consequences you can observe:
//!
//! - **TOML semantics are the shared baseline.** The value vocabulary is
//!   TOML's; formats that could express more (YAML custom tags, merge
//!   keys, ordered maps) do not get to, and formats that express less map into
//!   the baseline by explicit adapter rules. A config file means the same
//!   thing in every format: identical schema validation and strict-mode
//!   accept/reject decisions. (Unknown-key line numbers and source
//!   snippets are TOML-only today — YAML/JSON strict errors name the key
//!   and file but carry no source line.)
//! - **Datetimes cross formats by schema, not by sniffing.** TOML has
//!   first-class datetimes; in YAML and JSON they are written as strings
//!   in TOML's four datetime spellings (offset date-time, local
//!   date-time, local date, local time). A string becomes a
//!   [`value::Datetime`] only when the schema declares the leaf as one —
//!   adapters never guess types from a value's shape (no
//!   "looks like a date" detection, no YAML Norway problem).
//! - **Capabilities are declared, refusals are typed.** Not every format
//!   can support every operation honestly (comment-preserving edits are
//!   the canonical case). Each adapter declares its supported
//!   [`format::Operation`]s; asking for an undeclared one — or a declared
//!   one on a shape the format cannot rewrite honestly — yields the
//!   single typed [`format::UnsupportedByFormat`] error, never a silent
//!   lossy fallback. Call sites react to the error; nothing branches on
//!   format names.
//! - **Formats are opt-in and ordered.** Discovery by
//!   [`file_stem`](RuntimeBuilder::file_stem) enables the
//!   [`formats`](RuntimeBuilder::formats) list (default: TOML only, never
//!   inferred from cargo features); the first entry is the preferred
//!   format `config gen` renders and file seeding uses. The Discovery
//!   section below spells out the full file-name contract.
//!
//! # Core library — no CLI framework required
//!
//! The core of clapfig has **no dependency on any CLI framework**. Config
//! discovery, multi-file merging, environment variable mapping, key lookup,
//! persistence, and template generation all work through the builders and
//! [`ConfigAction`]. You can use clapfig in GUI apps, servers, embedded
//! tools, or with any argument parser.
//!
//! For [clap](https://docs.rs/clap) users, an optional adapter (the `cli`
//! module, behind the `clap` Cargo feature, on by default) provides drop-in
//! derive types that give your app `config gen|list|get|set|unset`
//! subcommands with zero boilerplate. To use clapfig without clap:
//!
//! ```toml
//! clapfig = { version = "...", default-features = false, features = ["derive"] }
//! ```
//!
//! # Layer precedence
//!
//! ```text
//! Compiled defaults     #[clapfig(default = ...)]
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
//! [`layer_order()`](RuntimeBuilder::layer_order) — for example, to make
//! files override env vars, or to exclude a layer entirely:
//!
//! ```ignore
//! Clapfig::schema_builder::<MyConfig>()
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
//! [`search_paths()`](RuntimeBuilder::search_paths) accepts a list of
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
//! *What* to look for in each directory is the file-name contract:
//!
//! - **[`file_name("myapp.toml")`](RuntimeBuilder::file_name)** — exact-name
//!   discovery. Only files with that precise name are considered, and the
//!   name's extension selects the single enabled format (an extensionless
//!   rc-style name parses as TOML). This is the default
//!   (`{app_name}.toml`), so TOML-only apps configure nothing.
//! - **[`file_stem("myapp")`](RuntimeBuilder::file_stem)** plus
//!   **[`formats([...])`](RuntimeBuilder::formats)** — stem discovery
//!   across the enabled formats' extensions (`myapp.toml`, `myapp.yaml` /
//!   `myapp.yml`, `myapp.json`). Formats are opt-in and ordered (default:
//!   TOML only); the first entry is the **preferred format** (`config gen`
//!   renders it; `config set` creates `<stem>.<preferred extension>` when
//!   no file exists). More than one same-stem match in the same directory
//!   is a hard error naming both files
//!   ([`ClapfigError::AmbiguousConfigFiles`]) — across directories, normal
//!   layering applies. Explicit paths (exact-name persist scopes,
//!   `gen --output`) select their adapter by extension, independent of
//!   the enabled list.
//!
//! ## Resolution — what to do with found files
//!
//! [`search_mode()`](RuntimeBuilder::search_mode) controls what happens
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
//! [`persist_scope()`](RuntimeBuilder::persist_scope) names a target for
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
//! use serde's `#[serde(deserialize_with = ...)]` on the field.
//!
//! Disable env loading entirely with [`.no_env()`](RuntimeBuilder::no_env)
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
//! [`layer_order()`](RuntimeBuilder::layer_order)). This is useful for WASM
//! frontends (Leptos, Dioxus, Yew) or server-side apps that accept config
//! overrides via the URL.
//!
//! ```ignore
//! let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
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
//! The [`cli_override()`](RuntimeBuilder::cli_override) and
//! [`cli_overrides_from()`](RuntimeBuilder::cli_overrides_from) methods
//! inject values at the `Cli` layer (highest priority by default). Despite
//! the name, they are not clap-specific — use them with any value source
//! (GUI inputs, HTTP headers, hardcoded test values). Their position in the
//! merge order can be changed with
//! [`layer_order()`](RuntimeBuilder::layer_order).
//!
//! `cli_overrides_from(source)` auto-matches: it serializes the source,
//! skips `None` values, and keeps only keys that exist in the schema.
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
//! doesn't match any field in your schema, loading fails with the file path,
//! key name, and — for TOML sources — the line number:
//!
//! ```text
//! Unknown key 'typo_key' in /home/user/.config/myapp/myapp.toml (line 5)
//! ```
//!
//! This catches typos and stale keys early. Turn it off with
//! [`.strict(false)`](RuntimeBuilder::strict) if you intentionally share
//! config files across tools or want forward-compatible configs.
//!
//! ## Cascading strictness
//!
//! Real apps rarely want one uniform answer. Typed fields want strict
//! ("catch my typos"); plugin-extension subtrees want lenient ("pass
//! through unknown keys"); some leaves inside that lenient subtree may
//! want to re-tighten. Three composable knobs cover all of that:
//!
//! 1. **`.strict(bool)`** — the whole-resolution default (see above).
//! 2. **`.strict_at(path, bool)`** — per-section override at a dotted path.
//!    Runtime schemas additionally set per-node strictness via
//!    [`Schema::strict(bool)`](crate::runtime::Schema::strict), and the
//!    derive path via `#[clapfig(strict = ...)]` on the struct.
//! 3. **`.on_unknown_key(callback)`** — last-word per-key callback that
//!    runs *only* on cascade-strict keys.
//!
//! ### The cascade rule
//!
//! For any unknown key at dotted path `a.b.c`, the effective strictness
//! is the value of the **nearest ancestor schema node** (including the
//! key's parent) whose `strict` is explicitly set. With no ancestor
//! override, the builder-level default applies.
//!
//! That single rule produces both expected behaviors:
//!
//! - A parent's `strict` cascades to every descendant that doesn't itself
//!   set `strict`.
//! - The first descendant that sets its own `strict` becomes the new root
//!   for its subtree, overriding the inherited value below it.
//!
//! ```ignore
//! Clapfig::schema_builder::<AppConfig>()
//!     .strict(true)                       // typo protection by default
//!     .strict_at("plugins", false)        // plugins.* subtree: lenient
//!     .strict_at("plugins.audit", true)   // …but plugins.audit re-tightens
//!     .load()?;
//! ```
//!
//! Targeting a leaf or an unknown path errors at
//! [`build_resolver()`](RuntimeBuilder::build_resolver) time with
//! [`ClapfigError::InvalidStrictPath`] — typo protection on the override
//! itself. With [`.normalize_keys(true)`](RuntimeBuilder::normalize_keys)
//! set, `path` may be written in kebab-case.
//!
//! ### The `on_unknown_key` callback
//!
//! Some shapes the cascade alone can't express: a struct level where
//! typed fields and a `#[serde(flatten)] BTreeMap` catch-all are
//! siblings. The cascade sees them as the same node — accept everything
//! or reject everything. The callback splits the difference: it runs
//! only on cascade-strict keys and decides each one. Returning
//! [`UnknownKeyDecision::Accept`] drops the key silently; `Reject`
//! produces a `ClapfigError::UnknownKeys` entry (the default).
//!
//! ```ignore
//! Clapfig::schema_builder::<AppConfig>()
//!     .strict(true)
//!     .on_unknown_key(|c: &UnknownKeyContext<'_>| {
//!         // Extension-emitted dotted keys (`"acme.task-due-date"`) are
//!         // a single TOML literal; bare typos aren't. Accept the
//!         // former, reject the latter.
//!         if c.leaf.contains('.') {
//!             UnknownKeyDecision::Accept
//!         } else {
//!             UnknownKeyDecision::Reject
//!         }
//!     })
//!     .load()?;
//! ```
//!
//! The [`UnknownKeyContext`] carries the dotted path, the raw leaf
//! key (preserves quoted-key semantics — `"acme.task-due-date-missing"`
//! stays as a single literal even though it contains dots), the parsed
//! value as `Option<&value::Value>` (`None` in the rare case lookup can't
//! resolve — out-of-bounds array index, path through a non-table
//! intermediate), the source file, and the 1-indexed line number
//! (`Some` on a best-effort match in TOML sources; always `None` for
//! YAML/JSON and non-file sources).
//!
//! # Runtime-defined schemas
//!
//! Some apps don't have a single compile-time config struct: plugin
//! hosts assemble their schema from loaded plugins, scripting hosts read
//! it from a config descriptor file, generated apps build it programmatically.
//! [`Clapfig::runtime`] is the entry point for those cases.
//!
//! ```ignore
//! use clapfig::{Clapfig, runtime::{Field, Schema}};
//!
//! let schema = Schema::object("App")
//!     .doc("Demo runtime schema.")
//!     .field("host", Field::string().doc("App host.").default("localhost"))
//!     .field("port", Field::integer().default(8080i64))
//!     .field(
//!         "level",
//!         Field::enum_of(["debug", "info", "warn", "error"])
//!             .doc("Log verbosity.")
//!             .default("info"),
//!     )
//!     .nested(
//!         "db",
//!         Schema::object("Db")
//!             .field("url", Field::string().optional())
//!             .field("pool_size", Field::integer().default(5i64)),
//!     )
//!     .build();
//!
//! let table: clapfig::value::Map = Clapfig::runtime(schema)
//!     .app_name("myapp")
//!     .load()?;
//! ```
//!
//! Same surface as [`Clapfig::schema_builder`] — `app_name`, `search_paths`,
//! `env_prefix`, `cli_override`, `post_validate`, `build_resolver`,
//! `handle` (drives `config gen|list|get|set|unset|schema`) — but the
//! result is a value [`Map`](value::Map) rather than a typed `C`, and
//! `post_validate` receives `&Map`.
//!
//! Both entry points produce identical schema metadata: the typed path's
//! derive macro emits the same [`runtime::Schema`] shape the runtime
//! builder constructs, so `config gen`, JSON Schema emission, persistence
//! validation, and strict-mode value context behave identically. The only
//! difference is that `Clapfig::schema_builder::<C>().load()` returns a
//! typed `C` while `Clapfig::runtime(schema).load()` returns a value
//! [`Map`](value::Map).
//!
//! `LeafType` covers the value model's primitives + array + map, plus
//! `Enum { values }` for constrained value sets (log levels, output
//! formats, modes). The schema is consumed by every surface with no extra
//! wiring: `config gen` emits a commented template with allowed-value
//! lines for enum leaves; the JSON Schema emitter emits `enum: [...]` on
//! the property; [`meta::doc_for_runtime`] reads doc-comment lines from
//! the schema.
//!
//! Cascading strictness composes naturally: schemas can set per-node
//! strictness inline via [`Schema::strict(bool)`](crate::runtime::Schema::strict),
//! and [`RuntimeBuilder::strict_at`](RuntimeBuilder::strict_at) /
//! [`RuntimeBuilder::on_unknown_key`](RuntimeBuilder::on_unknown_key)
//! overlay on top.
//!
//! Schema field names are validated at construction: empty names, names
//! containing `.` / `[` / `]`, and duplicate names within one schema
//! panic at `SchemaBuilder` time rather than producing silent
//! `KeyNotFound` errors at every consumer.
//!
//! # Kebab-case keys
//!
//! By default, keys in config files and overrides must match the Rust field
//! name exactly (`pool_size`, not `pool-size`). Opt into kebab acceptance
//! with [`.normalize_keys(true)`](RuntimeBuilder::normalize_keys):
//!
//! ```ignore
//! Clapfig::schema_builder::<MyConfig>()
//!     .app_name("myapp")
//!     .normalize_keys(true)
//!     .load()?;
//! ```
//!
//! Every key crossing the boundary into clapfig — TOML keys in files, dotted
//! keys in [`.cli_override()`](RuntimeBuilder::cli_override) /
//! [`.cli_overrides_from()`](RuntimeBuilder::cli_overrides_from), URL query
//! parameter keys — has its `-` characters rewritten to `_` before
//! validation, merging, and deserialization. So `pool-size`, `pool_size`,
//! and mixed forms all resolve to the same `pool_size` field.
//!
//! Strict-mode validation still flags genuine unknown keys; the error
//! message reports the normalized form, but the line-number snippet (TOML
//! sources) still points at the user's original line.
//!
//! Generated templates ([`ConfigAction::Gen`]) also follow the normalized
//! presentation: with `.normalize_keys(true)` on, `config gen` emits keys
//! and section headers in kebab-case (`pool-size`, `[my-section]`) so the
//! template matches what users will type. Doc comments and values are
//! never touched.
//!
//! The persistence path (`config set`/`unset`) and `config get` (merged
//! and scoped alike) follow the same acceptance: the action key may be
//! written in either spelling (it is normalized to the canonical
//! snake_case field for validation), edits land on the spelling already
//! present in the file (so setting `pool_size` against a kebab-case file
//! edits `pool-size` rather than creating a colliding duplicate), and
//! paths not yet present — including whole files seeded by `config set` —
//! are emitted kebab-case, matching `config gen` output. A file already
//! holding both equivalent spellings of a key is ambiguous: `set`,
//! `unset`, and scoped `get` fail with the same collision error loading
//! it reports, never silently picking one spelling.
//!
//! Environment variables are unaffected — shells dislike `-` in variable
//! names, and the env layer already lower-cases segments and treats `__` as
//! the nesting separator.
//!
//! # Semantic validation — the `post_validate` hook
//!
//! Strict mode and the schema's type checks together cover **structural**
//! validation: every key is known, every required field is present, every
//! value has the right type. They do not cover **semantic** constraints —
//! the rules that depend on the merged value rather than a single field's
//! type:
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
//! let config: AppConfig = Clapfig::schema_builder::<AppConfig>()
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
//! The hook runs after all layers have been merged and type-validated, but
//! before [`load()`](SchemaConfigBuilder::load) returns. Rejections become
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
//!   build a [`RuntimeResolver`] for tree-walk use cases (see the next
//!   section), the same hook runs on every
//!   [`resolve_at()`](RuntimeResolver::resolve_at) call.
//!
//! # Tree-walk resolution — the `RuntimeResolver` handle
//!
//! [`load()`](RuntimeBuilder::load) assumes one resolution per process,
//! anchored at `std::env::current_dir()`. For one-shot CLI tools that's
//! exactly right. But for tools that walk a **dynamic file tree** where every
//! directory can have an optional config — the `.htaccess` / `.gitignore` /
//! `.editorconfig` / `.eslintrc` pattern — you need N resolutions from N
//! different anchors, and you want to amortize the I/O cost across calls.
//!
//! [`RuntimeBuilder::build_resolver()`] gives you a reusable
//! [`RuntimeResolver`] handle for that case:
//!
//! ```ignore
//! let resolver = Clapfig::runtime(schema)
//!     .app_name("myapp")
//!     .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
//!     .post_validate(|t| validate_ranges(t))
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
//!   directory passed to [`resolve_at()`](RuntimeResolver::resolve_at)**, not
//!   at the process CWD. Each call is a fully independent resolution with the
//!   same builder state but a different starting point. Walking a site tree
//!   with `Ancestors(Boundary::Marker(".git"))` gives you
//!   nearest-project-config semantics on every leaf for free.
//! - **Files are cached by absolute path inside the resolver.** A tree walk
//!   that visits 1000 leaves sharing 5 ancestor config files pays the disk +
//!   parse cost once per unique file, not 1000×. The resolver is the cache
//!   scope — drop it to invalidate everything.
//! - **No mtime checking.** The cache is not invalidated when files change on
//!   disk. Long-lived processes that need freshness should build a new
//!   resolver. This is a deliberate "keep it simple" choice; the contract
//!   is documented and a regression test locks it in.
//! - **`load()` is the special case.** Internally `load()` is just
//!   `self.build_resolver()?.resolve_at(std::env::current_dir()?)`, so
//!   existing single-shot callers get zero behavior change and all resolution
//!   flows through one code path.
//! - **The `post_validate` hook composes naturally.** Registered once on the
//!   builder, captured into the resolver, fired on every `resolve_at` call —
//!   so per-leaf invariants get the same enforcement as top-level `load()`.
//!
//! See [`RuntimeResolver`] for the full API.
//!
//! # Normalizing values
//!
//! Use serde's `#[serde(deserialize_with = ...)]` on a field to normalize
//! values during the typed deserialize step. The function runs when the
//! merged table is deserialized into your `C` — covering values from any
//! source (config files, env vars, overrides, and schema defaults). This is
//! useful for case-insensitive fields, path canonicalization, or unit
//! conversion.
//!
//! # Template generation
//!
//! `config gen` (or [`ConfigAction::Gen`]) produces a documented config
//! file derived from the struct's `///` doc comments and
//! `#[clapfig(default)]` values, rendered in the preferred format (or, with
//! an output path, the format the path's extension selects). The template
//! stays in sync with code — change a doc comment or a default, the
//! template reflects it. Enum-typed fields get an
//! `Allowed: "a" | "b" | "c"` line; required fields without a default get a
//! commented placeholder hint. TOML and YAML carry docs as native comments;
//! JSON carries them as `"//"` comment keys (ADR-0002's convention: at most
//! one `"//"` per object, suffixed `"//field"` keys per field, arrays of
//! strings for multi-line prose), which the JSON adapter strips at parse
//! time so a generated template passes strict validation as-is. When
//! `config set` creates a new file, it seeds it from this template so the
//! user gets a documented starting point.
//!
//! # Metadata accessors
//!
//! Tools that build help text, tooltips, settings UIs, or `--describe`
//! flags can read a field's doc-comment lines directly from the schema —
//! no need to spin up the full resolve pipeline. See
//! [`meta::doc_for_runtime`]:
//!
//! ```ignore
//! let lines = clapfig::meta::doc_for_runtime(AppConfig::schema(), "database.pool-size")
//!     .unwrap_or_default();
//! ```
//!
//! Key spelling is lenient (kebab and snake are interchangeable), and the
//! function returns `None` for keys that don't exist so callers can
//! distinguish "no such field" from "field exists, undocumented."
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
//! - **Comment preservation**: edits go through the format adapter's
//!   comment-preserving editor, so users won't lose their annotations.
//!   Lossless for TOML (`toml_edit`); in JSON, comments are `"//"`-keyed
//!   data (ADR-0002) and survive edits for free, though formatting is
//!   normalized (pretty-printed, document key order preserved); YAML edits
//!   are targeted span patches, byte-preserving outside the edited span,
//!   and the shapes the patch stack cannot rewrite honestly (sequence-item
//!   replacement, flow-style list append) refuse with the typed
//!   [`format::UnsupportedByFormat`] error instead of corrupting the file.
//! - **Seeded files**: if the target file doesn't exist, a new one is created
//!   from the generated template, so the user gets doc comments for every
//!   field out of the box.
//! - **Validation before write**: `config set` validates that the key exists
//!   and the value matches the leaf's declared type before touching the
//!   file. A typo in the key name or a string where an integer is expected
//!   fails fast.
//! - **Scoped reads**: `config list --scope global` and `config get key
//!   --scope local` read from a single scope's file rather than the merged
//!   view, letting users inspect where values come from.
//!
//! # Error handling
//!
//! All fallible operations return [`ClapfigError`]. Errors are designed to
//! be user-facing: unknown keys include file paths (and, for TOML sources,
//! line numbers), unknown scopes list the available ones, and missing
//! prerequisites reference the builder method to call. See the [`error`]
//! module for the full set.

pub mod error;
pub mod format;
pub mod meta;
pub mod render;
pub mod runtime;
pub mod schema;
pub mod static_schema;
pub mod types;
pub mod value;

mod builder;
#[cfg(feature = "clap")]
mod cli;
mod env;
mod file;
mod flatten;
pub(crate) mod merge;
mod normalize;
mod ops;
mod overrides;
mod persist;
mod resolve;
mod runtime_builder;
mod runtime_spec;
mod schema_builder;
mod spec;
mod strict;
#[cfg(feature = "url")]
mod url;
mod validate;

#[cfg(test)]
mod fixtures;

pub use builder::Clapfig;
#[cfg(feature = "derive")]
pub use clapfig_derive::Schema;
#[cfg(feature = "clap")]
pub use cli::{ConfigArgs, ConfigCommand, ConfigSubcommand};
pub use error::{ClapfigError, UnknownKeyInfo};
pub use ops::ConfigResult;
pub use runtime_builder::{RuntimeBuilder, RuntimeResolver};
pub use schema_builder::SchemaConfigBuilder;
pub use static_schema::Schema;
pub use strict::{CollectedUnknown, UnknownKeyContext, UnknownKeyDecision};
pub use types::{Boundary, ConfigAction, Layer, SearchMode, SearchPath};
