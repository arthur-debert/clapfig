//! Rich, layered configuration for Rust CLI apps.
//!
//! Clapfig orchestrates configuration from multiple sources — config files,
//! environment variables, and CLI flags — with a builder API. Built on
//! [confique](https://docs.rs/confique) for struct-driven defaults and
//! template generation.
//!
//! # Layer precedence (lowest to highest)
//!
//! 1. Compiled defaults (`#[config(default = ...)]`)
//! 2. Config files (search paths in order, later paths win)
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
pub use types::{ConfigAction, SearchPath};
