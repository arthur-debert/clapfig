//! Configuration structs for the clapfig demo application.
//!
//! This module defines a multi-level config hierarchy to showcase clapfig's
//! support for nested configuration. The root [`DemoConfig`] contains two
//! nested sub-configs: [`ServerConfig`] and [`DisplayConfig`].
//!
//! Each struct derives [`confique::Config`] for defaults and template
//! generation, plus [`Serialize`]/[`Deserialize`] for the merge pipeline.
//!
//! # Env var mapping
//!
//! With the prefix `CLAPFIG_DEMO` (auto-derived from `app_name`), environment
//! variables map to dotted keys via double-underscore separators:
//!
//! | Env var                              | Config key             |
//! |--------------------------------------|------------------------|
//! | `CLAPFIG_DEMO__NAME`                 | `name`                 |
//! | `CLAPFIG_DEMO__VERBOSE`              | `verbose`              |
//! | `CLAPFIG_DEMO__SERVER__HOST`         | `server.host`          |
//! | `CLAPFIG_DEMO__SERVER__PORT`         | `server.port`          |
//! | `CLAPFIG_DEMO__SERVER__MAX_CONNECTIONS` | `server.max_connections` |
//! | `CLAPFIG_DEMO__DISPLAY__COLOR`       | `display.color`        |
//! | `CLAPFIG_DEMO__DISPLAY__FORMAT`      | `display.format`       |

use confique::Config;
use serde::{Deserialize, Serialize};

/// Root configuration for the demo application.
///
/// Contains top-level scalar keys and two nested sub-configs to demonstrate
/// clapfig's hierarchical merge across files, env vars, and CLI flags.
#[derive(Config, Serialize, Deserialize, Debug)]
pub struct DemoConfig {
    /// Application name shown in the echo banner.
    #[config(default = "clapfig-demo")]
    pub name: String,

    /// Enable verbose output.
    #[config(default = false)]
    pub verbose: bool,

    /// Server settings (nested config).
    #[config(nested)]
    pub server: ServerConfig,

    /// Display and formatting settings (nested config).
    #[config(nested)]
    pub display: DisplayConfig,
}

/// Server-related configuration.
///
/// Lives under the `[server]` section in TOML files and is accessed via
/// `server.*` dotted keys.
#[derive(Config, Serialize, Deserialize, Debug)]
pub struct ServerConfig {
    /// Hostname to bind to.
    #[config(default = "127.0.0.1")]
    pub host: String,

    /// Port number.
    #[config(default = 3000)]
    pub port: u16,

    /// Maximum number of allowed connections.
    #[config(default = 100)]
    pub max_connections: u32,
}

/// Display and output formatting configuration.
///
/// Lives under the `[display]` section in TOML files. The `color` key is
/// used by the `echo` command to colorize terminal output via ANSI codes.
#[derive(Config, Serialize, Deserialize, Debug)]
pub struct DisplayConfig {
    /// Terminal color for the echo command output.
    ///
    /// Supported values: red, green, yellow, blue, magenta, cyan, white.
    #[config(default = "yellow")]
    pub color: String,

    /// Output format (pretty or plain).
    #[config(default = "pretty")]
    pub format: String,
}
