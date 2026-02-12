//! Clap adapter for clapfig.
//!
//! This module is the **optional integration layer** between clapfig's
//! framework-agnostic core and the [clap](https://docs.rs/clap) CLI parser.
//! It is compiled only when the `clap` Cargo feature is enabled (on by
//! default).
//!
//! The module provides two clap derive types — [`ConfigArgs`] and
//! [`ConfigSubcommand`] — that you can embed directly into your clap
//! `#[derive(Parser)]` struct to get `config gen|list|get|set` subcommands
//! with no boilerplate.
//!
//! The only bridge to the core is [`ConfigArgs::into_action()`], which
//! converts clap-parsed arguments into a [`ConfigAction`](crate::ConfigAction).
//! From there, all logic flows through the clap-free
//! [`ClapfigBuilder::handle()`](crate::ClapfigBuilder::handle) API.
//!
//! If you use a different CLI parser (or no CLI at all), you can skip this
//! module entirely and construct [`ConfigAction`](crate::ConfigAction) values
//! directly.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::types::ConfigAction;

/// Clap-derived args for the `config` subcommand group.
///
/// Embed this into your app's clap derive:
/// ```ignore
/// #[derive(Parser)]
/// struct Cli {
///     #[command(subcommand)]
///     command: Commands,
/// }
///
/// #[derive(Subcommand)]
/// enum Commands {
///     Config(ConfigArgs),
/// }
/// ```
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigSubcommand>,
}

/// Available config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
    /// Show all resolved configuration key-value pairs.
    List,
    /// Generate a commented sample configuration file.
    Gen {
        /// Write to a file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show the resolved value and documentation for a config key.
    Get {
        /// Dotted key path (e.g. "database.url").
        key: String,
    },
    /// Persist a configuration value to the config file.
    Set {
        /// Dotted key path (e.g. "database.url").
        key: String,
        /// Value to set.
        value: String,
    },
}

impl ConfigArgs {
    /// Convert clap-parsed args into a framework-agnostic `ConfigAction`.
    ///
    /// Bare `config` (no subcommand) and explicit `config list` both map to
    /// `ConfigAction::List`.
    pub fn into_action(self) -> ConfigAction {
        match self.action {
            None | Some(ConfigSubcommand::List) => ConfigAction::List,
            Some(ConfigSubcommand::Gen { output }) => ConfigAction::Gen { output },
            Some(ConfigSubcommand::Get { key }) => ConfigAction::Get { key },
            Some(ConfigSubcommand::Set { key, value }) => ConfigAction::Set { key, value },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so we can use `try_parse_from` on the subcommand.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        config: ConfigArgs,
    }

    fn parse(args: &[&str]) -> ConfigArgs {
        TestCli::try_parse_from(args).unwrap().config
    }

    #[test]
    fn parse_gen_no_output() {
        let args = parse(&["test", "gen"]);
        let action = args.into_action();
        assert_eq!(action, ConfigAction::Gen { output: None });
    }

    #[test]
    fn parse_gen_with_output() {
        let args = parse(&["test", "gen", "-o", "out.toml"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }

    #[test]
    fn parse_gen_with_long_output() {
        let args = parse(&["test", "gen", "--output", "/etc/myapp.toml"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Gen {
                output: Some(PathBuf::from("/etc/myapp.toml"))
            }
        );
    }

    #[test]
    fn parse_get() {
        let args = parse(&["test", "get", "database.url"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Get {
                key: "database.url".into()
            }
        );
    }

    #[test]
    fn parse_set() {
        let args = parse(&["test", "set", "port", "3000"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into()
            }
        );
    }

    #[test]
    fn parse_set_string_value() {
        let args = parse(&["test", "set", "host", "0.0.0.0"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Set {
                key: "host".into(),
                value: "0.0.0.0".into()
            }
        );
    }

    #[test]
    fn invalid_subcommand_errors() {
        let result = TestCli::try_parse_from(["test", "nope"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_bare_config_is_list() {
        let args = parse(&["test"]);
        let action = args.into_action();
        assert_eq!(action, ConfigAction::List);
    }

    #[test]
    fn parse_explicit_list() {
        let args = parse(&["test", "list"]);
        let action = args.into_action();
        assert_eq!(action, ConfigAction::List);
    }
}
