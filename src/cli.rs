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

use clap::{Arg, ArgMatches, Args, Command, Subcommand};

use crate::error::ClapfigError;
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

/// Runtime-configurable alternative to [`ConfigArgs`] for apps that need
/// to rename subcommands or flags to avoid conflicts.
///
/// Both `ConfigArgs` (derive) and `ConfigCommand` (builder) produce
/// [`ConfigAction`], so all downstream logic is shared.
///
/// # Example
///
/// ```ignore
/// use clapfig::ConfigCommand;
///
/// let config_cmd = ConfigCommand::new()
///     .scope_long("target")       // rename --scope to --target
///     .gen_name("template");      // rename "gen" to "template"
///
/// let app = Cli::command()
///     .subcommand(config_cmd.as_command("settings"));
///
/// let matches = app.get_matches();
/// if let Some(("settings", sub)) = matches.subcommand() {
///     let action = config_cmd.parse(sub)?;
///     builder.handle_and_print(&action)?;
/// }
/// ```
pub struct ConfigCommand {
    list_name: String,
    gen_name: String,
    get_name: String,
    set_name: String,
    unset_name: String,
    scope_long: String,
    output_long: String,
    output_short: Option<char>,
}

impl Default for ConfigCommand {
    fn default() -> Self {
        Self {
            list_name: "list".into(),
            gen_name: "gen".into(),
            get_name: "get".into(),
            set_name: "set".into(),
            unset_name: "unset".into(),
            scope_long: "scope".into(),
            output_long: "output".into(),
            output_short: Some('o'),
        }
    }
}

impl ConfigCommand {
    /// Create a new `ConfigCommand` with default names matching [`ConfigArgs`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Rename the `list` subcommand.
    pub fn list_name(mut self, name: impl Into<String>) -> Self {
        self.list_name = name.into();
        self
    }

    /// Rename the `gen` subcommand.
    pub fn gen_name(mut self, name: impl Into<String>) -> Self {
        self.gen_name = name.into();
        self
    }

    /// Rename the `get` subcommand.
    pub fn get_name(mut self, name: impl Into<String>) -> Self {
        self.get_name = name.into();
        self
    }

    /// Rename the `set` subcommand.
    pub fn set_name(mut self, name: impl Into<String>) -> Self {
        self.set_name = name.into();
        self
    }

    /// Rename the `unset` subcommand.
    pub fn unset_name(mut self, name: impl Into<String>) -> Self {
        self.unset_name = name.into();
        self
    }

    /// Rename the `--scope` flag.
    pub fn scope_long(mut self, name: impl Into<String>) -> Self {
        self.scope_long = name.into();
        self
    }

    /// Rename the `--output` flag on the `gen` subcommand.
    pub fn output_long(mut self, name: impl Into<String>) -> Self {
        self.output_long = name.into();
        self
    }

    /// Set or disable the short flag for `--output` (default: `Some('o')`).
    /// Pass `None` to remove the short flag entirely.
    pub fn output_short(mut self, short: Option<char>) -> Self {
        self.output_short = short;
        self
    }

    /// Build a [`clap::Command`] with the configured names.
    ///
    /// The `name` parameter sets the top-level subcommand name
    /// (e.g. `"config"`, `"settings"`).
    pub fn as_command(&self, name: &str) -> Command {
        let scope_arg = Arg::new("scope")
            .long(self.scope_long.clone())
            .help("Target a named persist scope (e.g. \"local\", \"global\").")
            .global(true);

        let mut output_arg = Arg::new("output")
            .long(self.output_long.clone())
            .help("Write to a file instead of stdout.")
            .value_parser(clap::value_parser!(PathBuf));
        if let Some(short) = self.output_short {
            output_arg = output_arg.short(short);
        }

        let list_cmd = Command::new(self.list_name.clone())
            .about("Show all resolved configuration key-value pairs.");

        let gen_cmd = Command::new(self.gen_name.clone())
            .about("Generate a commented sample configuration file.")
            .arg(output_arg);

        let get_cmd = Command::new(self.get_name.clone())
            .about("Show the resolved value and documentation for a config key.")
            .arg(
                Arg::new("key")
                    .required(true)
                    .help("Dotted key path (e.g. \"database.url\")."),
            );

        let set_cmd = Command::new(self.set_name.clone())
            .about("Persist a configuration value to the config file.")
            .arg(
                Arg::new("key")
                    .required(true)
                    .help("Dotted key path (e.g. \"database.url\")."),
            )
            .arg(Arg::new("value").required(true).help("Value to set."));

        let unset_cmd = Command::new(self.unset_name.clone())
            .about("Remove a configuration value from the config file.")
            .arg(
                Arg::new("key")
                    .required(true)
                    .help("Dotted key path (e.g. \"database.url\")."),
            );

        Command::new(name.to_owned())
            .about("Manage configuration.")
            .subcommand_required(false)
            .arg(scope_arg)
            .subcommand(list_cmd)
            .subcommand(gen_cmd)
            .subcommand(get_cmd)
            .subcommand(set_cmd)
            .subcommand(unset_cmd)
    }

    /// Extract a [`ConfigAction`] from parsed [`ArgMatches`].
    ///
    /// Bare invocation (no subcommand) maps to `ConfigAction::List`,
    /// matching the behavior of [`ConfigArgs::into_action`].
    pub fn parse(&self, matches: &ArgMatches) -> Result<ConfigAction, ClapfigError> {
        let scope = matches.get_one::<String>("scope").cloned();

        match matches.subcommand() {
            None => Ok(ConfigAction::List { scope }),
            Some((name, _)) if name == self.list_name => Ok(ConfigAction::List { scope }),
            Some((name, sub)) if name == self.gen_name => {
                let output = sub.get_one::<PathBuf>("output").cloned();
                Ok(ConfigAction::Gen { output })
            }
            Some((name, sub)) if name == self.get_name => {
                let key = sub.get_one::<String>("key").unwrap().clone();
                Ok(ConfigAction::Get { key, scope })
            }
            Some((name, sub)) if name == self.set_name => {
                let key = sub.get_one::<String>("key").unwrap().clone();
                let value = sub.get_one::<String>("value").unwrap().clone();
                Ok(ConfigAction::Set { key, value, scope })
            }
            Some((name, sub)) if name == self.unset_name => {
                let key = sub.get_one::<String>("key").unwrap().clone();
                Ok(ConfigAction::Unset { key, scope })
            }
            Some((name, _)) => Err(ClapfigError::UnknownSubcommand(name.to_owned())),
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

    // =======================================================================
    // ConfigCommand tests
    // =======================================================================

    /// Helper: build a top-level Command wrapping ConfigCommand, parse, and
    /// return the ConfigAction.
    fn cmd_parse(cmd: &ConfigCommand, args: &[&str]) -> ConfigAction {
        let app = Command::new("test").subcommand(cmd.as_command("config"));
        let matches = app.try_get_matches_from(args).unwrap();
        let (_, sub) = matches.subcommand().unwrap();
        cmd.parse(sub).unwrap()
    }

    // --- default names (should match ConfigArgs behavior) ---

    #[test]
    fn cmd_default_bare_is_list() {
        let cmd = ConfigCommand::new();
        let app = Command::new("test").subcommand(cmd.as_command("config"));
        let matches = app.try_get_matches_from(["test", "config"]).unwrap();
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(cmd.parse(sub).unwrap(), ConfigAction::List { scope: None });
    }

    #[test]
    fn cmd_default_list() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "list"]),
            ConfigAction::List { scope: None }
        );
    }

    #[test]
    fn cmd_default_gen() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen"]),
            ConfigAction::Gen { output: None }
        );
    }

    #[test]
    fn cmd_default_gen_with_output() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen", "-o", "out.toml"]),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }

    #[test]
    fn cmd_default_gen_with_long_output() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen", "--output", "out.toml"]),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }

    #[test]
    fn cmd_default_get() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "get", "database.url"]),
            ConfigAction::Get {
                key: "database.url".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_default_set() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "set", "port", "3000"]),
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_default_unset() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "unset", "port"]),
            ConfigAction::Unset {
                key: "port".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_default_scope_flag() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(
                &cmd,
                &["test", "config", "--scope", "global", "get", "port"]
            ),
            ConfigAction::Get {
                key: "port".into(),
                scope: Some("global".into()),
            }
        );
    }

    // --- renamed subcommands ---

    #[test]
    fn cmd_renamed_get() {
        let cmd = ConfigCommand::new().get_name("read");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "read", "database.url"]),
            ConfigAction::Get {
                key: "database.url".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_renamed_set() {
        let cmd = ConfigCommand::new().set_name("write");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "write", "port", "3000"]),
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_renamed_unset() {
        let cmd = ConfigCommand::new().unset_name("remove");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "remove", "port"]),
            ConfigAction::Unset {
                key: "port".into(),
                scope: None,
            }
        );
    }

    #[test]
    fn cmd_renamed_list() {
        let cmd = ConfigCommand::new().list_name("show");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "show"]),
            ConfigAction::List { scope: None }
        );
    }

    #[test]
    fn cmd_renamed_gen() {
        let cmd = ConfigCommand::new().gen_name("template");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "template"]),
            ConfigAction::Gen { output: None }
        );
    }

    // --- renamed flags ---

    #[test]
    fn cmd_renamed_scope_flag() {
        let cmd = ConfigCommand::new().scope_long("target");
        assert_eq!(
            cmd_parse(
                &cmd,
                &["test", "config", "--target", "global", "get", "port"]
            ),
            ConfigAction::Get {
                key: "port".into(),
                scope: Some("global".into()),
            }
        );
    }

    #[test]
    fn cmd_renamed_output_long() {
        let cmd = ConfigCommand::new().output_long("file");
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen", "--file", "out.toml"]),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }

    #[test]
    fn cmd_renamed_output_short() {
        let cmd = ConfigCommand::new().output_short(Some('f'));
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen", "-f", "out.toml"]),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }

    #[test]
    fn cmd_disabled_output_short() {
        let cmd = ConfigCommand::new().output_short(None);
        // Long form still works
        assert_eq!(
            cmd_parse(&cmd, &["test", "config", "gen", "--output", "out.toml"]),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
        // Short form should fail
        let app = Command::new("test").subcommand(cmd.as_command("config"));
        assert!(
            app.try_get_matches_from(["test", "config", "gen", "-o", "out.toml"])
                .is_err()
        );
    }

    // --- scope positioning ---

    #[test]
    fn cmd_scope_after_subcommand() {
        let cmd = ConfigCommand::new();
        assert_eq!(
            cmd_parse(
                &cmd,
                &["test", "config", "set", "port", "3000", "--scope", "global"]
            ),
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: Some("global".into()),
            }
        );
    }

    // --- custom top-level name ---

    #[test]
    fn cmd_custom_top_level_name() {
        let cmd = ConfigCommand::new();
        let app = Command::new("test").subcommand(cmd.as_command("settings"));
        let matches = app
            .try_get_matches_from(["test", "settings", "get", "port"])
            .unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(name, "settings");
        assert_eq!(
            cmd.parse(sub).unwrap(),
            ConfigAction::Get {
                key: "port".into(),
                scope: None,
            }
        );
    }

    // --- multiple renames composed ---

    #[test]
    fn cmd_all_renamed() {
        let cmd = ConfigCommand::new()
            .list_name("show")
            .gen_name("template")
            .get_name("read")
            .set_name("write")
            .unset_name("remove")
            .scope_long("target")
            .output_long("file")
            .output_short(Some('f'));

        let app = Command::new("test").subcommand(cmd.as_command("settings"));

        // write with --target
        let m = app
            .clone()
            .try_get_matches_from([
                "test", "settings", "--target", "global", "write", "port", "3000",
            ])
            .unwrap();
        let (_, sub) = m.subcommand().unwrap();
        assert_eq!(
            cmd.parse(sub).unwrap(),
            ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: Some("global".into()),
            }
        );

        // template with -f
        let m = app
            .try_get_matches_from(["test", "settings", "template", "-f", "out.toml"])
            .unwrap();
        let (_, sub) = m.subcommand().unwrap();
        assert_eq!(
            cmd.parse(sub).unwrap(),
            ConfigAction::Gen {
                output: Some(PathBuf::from("out.toml"))
            }
        );
    }
}
