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
    pub action: ConfigSubcommand,
}

/// Available config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
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
    pub fn into_action(self) -> ConfigAction {
        match self.action {
            ConfigSubcommand::Gen { output } => ConfigAction::Gen { output },
            ConfigSubcommand::Get { key } => ConfigAction::Get { key },
            ConfigSubcommand::Set { key, value } => ConfigAction::Set { key, value },
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
        let result = TestCli::try_parse_from(&["test", "nope"]);
        assert!(result.is_err());
    }
}
