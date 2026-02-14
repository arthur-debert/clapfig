//! Rich, layered configuration for Rust applications.
//!
//! Clapfig manages configuration from multiple sources — config files,
//! environment variables, and programmatic overrides — with a builder API that
//! takes a few lines to set up. Built on [confique](https://docs.rs/confique)
//! for struct-driven defaults and template generation.
//!
//! # Core library — no CLI framework required
//!
//! The core of clapfig is a **pure Rust API** with no dependency on any CLI
//! framework. Config discovery, multi-file merging, environment variable
//! mapping, key lookup, persistence, and template generation all work through
//! [`ClapfigBuilder`] and [`ConfigAction`] without importing clap or any other
//! parser. You can use clapfig in GUI apps, servers, or with any CLI parser of
//! your choice.
//!
//! # Optional clap adapter (`clap` feature, on by default)
//!
//! For CLI apps using [clap](https://docs.rs/clap), clapfig ships an optional
//! adapter behind the **`clap`** Cargo feature (enabled by default). The [`cli`]
//! module provides [`ConfigArgs`](cli::ConfigArgs) and
//! [`ConfigSubcommand`](cli::ConfigSubcommand) — ready-made clap derive types
//! that give your app `config gen|list|get|set|unset` subcommands with zero
//! boilerplate. They convert to the framework-agnostic [`ConfigAction`] via
//! [`ConfigArgs::into_action()`](cli::ConfigArgs::into_action).
//!
//! To use clapfig **without** clap, disable default features:
//!
//! ```toml
//! clapfig = { version = "...", default-features = false }
//! ```
//!
//! # Three axes of configuration
//!
//! Config file handling is controlled by three orthogonal settings on the builder:
//!
//! - **Discovery** ([`search_paths()`](ClapfigBuilder::search_paths)): where to
//!   look for config files. Supports explicit directories, platform paths, and
//!   walking up the directory tree via [`SearchPath::Ancestors`](types::SearchPath::Ancestors).
//!
//! - **Resolution** ([`search_mode()`](ClapfigBuilder::search_mode)): what to do
//!   with found files. [`Merge`](types::SearchMode::Merge) deep-merges all found
//!   configs (layered overrides); [`FirstMatch`](types::SearchMode::FirstMatch)
//!   uses only the highest-priority file found ("find my config").
//!
//! - **Persistence** ([`persist_scope()`](ClapfigBuilder::persist_scope)): named
//!   targets for `config set` writes. Scope paths are auto-added to search paths.
//!
//! See the [`types`] module for the full conceptual documentation and use-case
//! examples.
//!
//! # Layer precedence (lowest to highest)
//!
//! 1. Compiled defaults (`#[config(default = ...)]`)
//! 2. Config files (discovery + resolution mode)
//! 3. Environment variables (`PREFIX__KEY`)
//! 4. Programmatic overrides (`.cli_override()`)
//!
//! Every layer is sparse — only the keys you specify are merged.
//!
//! # Quick start
//!
//! ```ignore
//! let config: AppConfig = Clapfig::builder()
//!     .app_name("myapp")
//!     .load()?;
//! ```

pub mod error;
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
mod validate;

#[cfg(test)]
mod fixtures;

pub use builder::{Clapfig, ClapfigBuilder};
#[cfg(feature = "clap")]
pub use cli::{ConfigArgs, ConfigCommand, ConfigSubcommand};
pub use error::ClapfigError;
pub use ops::ConfigResult;
pub use types::{Boundary, ConfigAction, SearchMode, SearchPath};
