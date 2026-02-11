pub mod error;
pub mod types;

mod builder;
mod cli;
mod env;
mod file;
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
