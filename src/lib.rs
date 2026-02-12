//! Rich, layered configuration for Rust CLI apps.
//!
//! Clapfig orchestrates configuration from multiple sources — config files,
//! environment variables, and CLI flags — with a builder API. Built on
//! [confique](https://docs.rs/confique) for struct-driven defaults and
//! template generation.
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
//! - **Persistence** ([`persist_path()`](ClapfigBuilder::persist_path)): where
//!   `config set` writes. Explicit and independent of the search paths — no
//!   guessing.
//!
//! See the [`types`] module for the full conceptual documentation and use-case
//! examples.
//!
//! # Layer precedence (lowest to highest)
//!
//! 1. Compiled defaults (`#[config(default = ...)]`)
//! 2. Config files (discovery + resolution mode)
//! 3. Environment variables (`PREFIX__KEY`)
//! 4. CLI overrides (`.cli_override()`)
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
pub use cli::{ConfigArgs, ConfigSubcommand};
pub use error::ClapfigError;
pub use ops::ConfigResult;
pub use types::{Boundary, ConfigAction, SearchMode, SearchPath};
