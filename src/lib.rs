pub mod error;
pub mod types;

pub mod merge;
mod env;
mod overrides;
mod validate;
mod file;
mod resolve;
mod ops;
mod persist;
mod builder;
mod cli;

#[cfg(test)]
mod fixtures;

pub use builder::{Clapfig, ClapfigBuilder};
pub use cli::{ConfigArgs, ConfigSubcommand};
pub use error::ClapfigError;
pub use ops::ConfigResult;
pub use types::{ConfigAction, Format, SearchPath};
