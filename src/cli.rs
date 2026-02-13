//! Clap adapter for clapfig.
//!
//! This module is the **optional integration layer** between clapfig's
//! framework-agnostic core and the [clap](https://docs.rs/clap) CLI parser.
//! It is compiled only when the `clap` Cargo feature is enabled (on by
//! default).
//!
//! The module provides two clap derive types — [`ConfigArgs`] and
//! [`ConfigSubcommand`] — that you can embed directly into your clap
//! `#[derive(Parser)]` struct to get `config gen|list|get|set|unset` subcommands
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
    /// Target a named persist scope (e.g. "local", "global").
    ///
    /// For `set`/`unset`: selects which config file to write to. Defaults to the
    /// first scope configured on the builder.
    ///
    /// For `list`/`get`: reads from that scope's config file only (instead of
    /// the merged resolved view).
    #[arg(long, global = true)]
    pub scope: Option<String>,

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
    /// Remove a configuration value from the config file.
    Unset {
        /// Dotted key path (e.g. "database.url").
        key: String,
    },
}

impl ConfigArgs {
    /// Convert clap-parsed args into a framework-agnostic `ConfigAction`.
    ///
    /// Bare `config` (no subcommand) and explicit `config list` both map to
    /// `ConfigAction::List`. The `--scope` flag is threaded through to all
    /// variants except `Gen`.
    pub fn into_action(self) -> ConfigAction {
        let scope = self.scope;
        match self.action {
            None | Some(ConfigSubcommand::List) => ConfigAction::List { scope },
            Some(ConfigSubcommand::Gen { output }) => ConfigAction::Gen { output },
            Some(ConfigSubcommand::Get { key }) => ConfigAction::Get { key, scope },
            Some(ConfigSubcommand::Set { key, value }) => ConfigAction::Set { key, value, scope },
            Some(ConfigSubcommand::Unset { key }) => ConfigAction::Unset { key, scope },
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
                key: "database.url".into(),
                scope: None,
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
                value: "3000".into(),
                scope: None,
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
                value: "0.0.0.0".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn invalid_subcommand_errors() {
        let result = TestCli::try_parse_from(["test", "nope"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_unset() {
        let args = parse(&["test", "unset", "database.url"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Unset {
                key: "database.url".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn parse_bare_config_is_list() {
        let args = parse(&["test"]);
        let action = args.into_action();
        assert_eq!(action, ConfigAction::List { scope: None });
    }

    #[test]
    fn parse_explicit_list() {
        let args = parse(&["test", "list"]);
        let action = args.into_action();
        assert_eq!(action, ConfigAction::List { scope: None });
    }

    // --- scope flag tests ---

    #[test]
    fn parse_set_with_scope() {
        let args = parse(&["test", "set", "port", "3000", "--scope", "global"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: Some("global".into()),
            }
        );
    }

    #[test]
    fn parse_scope_before_subcommand() {
        let args = parse(&["test", "--scope", "global", "set", "port", "3000"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: Some("global".into()),
            }
        );
    }

    #[test]
    fn parse_list_with_scope() {
        let args = parse(&["test", "list", "--scope", "global"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::List {
                scope: Some("global".into()),
            }
        );
    }

    #[test]
    fn parse_get_with_scope() {
        let args = parse(&["test", "get", "port", "--scope", "local"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Get {
                key: "port".into(),
                scope: Some("local".into()),
            }
        );
    }

    #[test]
    fn parse_unset_with_scope() {
        let args = parse(&["test", "unset", "port", "--scope", "global"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::Unset {
                key: "port".into(),
                scope: Some("global".into()),
            }
        );
    }

    #[test]
    fn parse_bare_config_with_scope() {
        let args = parse(&["test", "--scope", "global"]);
        let action = args.into_action();
        assert_eq!(
            action,
            ConfigAction::List {
                scope: Some("global".into()),
            }
        );
    }
}
