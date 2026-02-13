//! # clapfig demo application
//!
//! A sample CLI tool that showcases how to integrate
//! [clapfig](https://docs.rs/clapfig) into a real application. This is **not**
//! a real app — it exists purely to demonstrate and manually verify clapfig's
//! features.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example clapfig_demo -- echo
//! cargo run --example clapfig_demo -- config list
//! ```
//!
//! ## Features demonstrated
//!
//! | Feature                  | How to exercise it                                                  |
//! |--------------------------|---------------------------------------------------------------------|
//! | Compiled defaults        | `cargo run --example clapfig_demo -- echo`                          |
//! | Config file (cwd)        | Create `clapfig-demo.toml` in cwd, then run `echo`                  |
//! | Config file (XDG/home)   | Place file under `~/.clapfig-demo/` or platform config dir          |
//! | Env var override         | `CLAPFIG_DEMO__DISPLAY__COLOR=red cargo run --example clapfig_demo -- echo` |
//! | Nested env var           | `CLAPFIG_DEMO__SERVER__PORT=9999 cargo run --example clapfig_demo -- echo`  |
//! | CLI override (top-level) | `cargo run --example clapfig_demo -- --verbose echo`                |
//! | CLI override (nested)    | `cargo run --example clapfig_demo -- --color blue echo`             |
//! | `config gen`             | `cargo run --example clapfig_demo -- config gen`                    |
//! | `config get`             | `cargo run --example clapfig_demo -- config get server.port`        |
//! | `config set`             | `cargo run --example clapfig_demo -- config set server.port 8080`   |
//! | `config list`            | `cargo run --example clapfig_demo -- config list`                   |
//! | Single key echo          | `cargo run --example clapfig_demo -- echo --key display.color`      |
//! | Colored output           | Default is yellow; override `display.color` to change it            |

mod config;

use clap::{Parser, Subcommand};
use serde::Serialize;

use clapfig::{Clapfig, ClapfigBuilder, ConfigArgs, SearchPath};

use config::DemoConfig;

// ---------------------------------------------------------------------------
// CLI definitions
// ---------------------------------------------------------------------------

/// clapfig demo — a sample CLI app for showcasing clapfig integration.
#[derive(Parser, Debug)]
#[command(name = "clapfig-demo")]
struct Cli {
    /// Enable verbose output.
    #[arg(long, global = true)]
    verbose: bool,

    /// Override the display color (red, green, yellow, blue, magenta, cyan).
    #[arg(long, global = true)]
    color: Option<String>,

    /// Override the server host.
    #[arg(long, global = true)]
    host: Option<String>,

    /// Override the server port.
    #[arg(long, global = true)]
    port: Option<u16>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print resolved configuration values (colored by display.color).
    Echo {
        /// Print only this dotted key instead of all values.
        #[arg(long)]
        key: Option<String>,
    },
    /// Manage the configuration file (gen, get, set, list).
    Config(ConfigArgs),
}

/// Serializable projection of CLI flags for [`ClapfigBuilder::cli_overrides_from`].
///
/// Only includes fields whose names match top-level config keys.
/// `cli_overrides_from` auto-matches by field name and silently ignores
/// the rest, so we only need `verbose` here (it matches `DemoConfig::verbose`).
/// The other CLI flags (`color`, `host`, `port`) map to *nested* config keys
/// and must be wired with [`ClapfigBuilder::cli_override`] instead.
#[derive(Serialize)]
struct CliOverrides {
    verbose: bool,
}

// ---------------------------------------------------------------------------
// Builder helper
// ---------------------------------------------------------------------------

/// Create a [`ClapfigBuilder`] wired up for the demo app.
///
/// Search paths: Platform (XDG / Library) → `~/.clapfig-demo/` → cwd.
/// Env prefix: `CLAPFIG_DEMO` (auto-derived).
///
/// CLI overrides are applied in two ways to showcase both clapfig methods:
///
/// 1. **`cli_overrides_from`** — auto-matches top-level keys (`verbose`)
///    by serializing a struct and keeping only fields that match config keys.
/// 2. **`cli_override`** — manually maps `--color` → `display.color`,
///    `--host` → `server.host`, `--port` → `server.port` (nested keys that
///    don't match by flat name).
fn make_builder(cli: &Cli) -> ClapfigBuilder<DemoConfig> {
    let overrides = CliOverrides {
        verbose: cli.verbose,
    };

    Clapfig::builder()
        .app_name("clapfig-demo")
        .env_prefix("CLAPFIG_DEMO")
        .search_paths(vec![
            SearchPath::Platform,
            SearchPath::Home(".clapfig-demo"),
            SearchPath::Cwd,
        ])
        .persist_scope("local", SearchPath::Home(".clapfig-demo"))
        // Auto-match top-level keys.
        .cli_overrides_from(&overrides)
        // Manually map nested keys.
        .cli_override("display.color", cli.color.clone())
        .cli_override("server.host", cli.host.clone())
        .cli_override("server.port", cli.port.map(i64::from))
}

// ---------------------------------------------------------------------------
// ANSI color helpers
// ---------------------------------------------------------------------------

fn ansi_color_code(name: &str) -> &str {
    match name {
        "red" => "\x1b[31m",
        "green" => "\x1b[32m",
        "yellow" => "\x1b[33m",
        "blue" => "\x1b[34m",
        "magenta" => "\x1b[35m",
        "cyan" => "\x1b[36m",
        "white" => "\x1b[37m",
        _ => "\x1b[0m",
    }
}

const RESET: &str = "\x1b[0m";

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn echo_all(config: &DemoConfig) {
    let color = ansi_color_code(&config.display.color);

    if config.verbose {
        println!(
            "{color}[verbose] Resolved configuration for {:?}{RESET}",
            config.name
        );
        println!();
    }

    let entries = [
        ("name", config.name.as_str()),
        ("verbose", &config.verbose.to_string()),
        ("server.host", config.server.host.as_str()),
        ("server.port", &config.server.port.to_string()),
        (
            "server.max_connections",
            &config.server.max_connections.to_string(),
        ),
        ("display.color", config.display.color.as_str()),
        ("display.format", config.display.format.as_str()),
    ];

    if config.display.format == "plain" {
        for (key, value) in &entries {
            println!("{key}={value}");
        }
    } else {
        let max_key_len = entries.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (key, value) in &entries {
            println!("{color}{key:<max_key_len$}{RESET}  {value}");
        }
    }
}

fn echo_key(config: &DemoConfig, key: &str) {
    let color = ansi_color_code(&config.display.color);
    let value: String = match key {
        "name" => config.name.clone(),
        "verbose" => config.verbose.to_string(),
        "server.host" => config.server.host.clone(),
        "server.port" => config.server.port.to_string(),
        "server.max_connections" => config.server.max_connections.to_string(),
        "display.color" => config.display.color.clone(),
        "display.format" => config.display.format.clone(),
        _ => {
            eprintln!("Unknown key: {key}");
            std::process::exit(1);
        }
    };
    println!("{color}{key}{RESET}  {value}");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();
    let builder = make_builder(&cli);

    match cli.command {
        Commands::Echo { key } => {
            let config = builder.load().unwrap_or_else(|e| {
                eprintln!("Failed to load config:\n{e}");
                std::process::exit(1);
            });
            match key {
                Some(k) => echo_key(&config, &k),
                None => echo_all(&config),
            }
        }
        Commands::Config(args) => {
            let action = args.into_action();
            builder.handle_and_print(&action).unwrap_or_else(|e| {
                eprintln!("Config error:\n{e}");
                std::process::exit(1);
            });
        }
    }
}
