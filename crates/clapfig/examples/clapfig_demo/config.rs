//! Configuration structs for the clapfig demo application.
//!
//! This module defines a multi-level config hierarchy to showcase clapfig's
//! support for nested configuration. The root [`DemoConfig`] contains two
//! nested sub-configs: [`ServerConfig`] and [`DisplayConfig`].
//!
//! Each struct derives [`clapfig::Schema`] for defaults, type metadata, and
//! template generation, plus [`Serialize`]/[`Deserialize`] for the merge
//! pipeline. [`Color`] and [`OutputFormat`] are unit-only enums so
//! `config gen` emits `Allowed:` lines instead of prose.
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

use clapfig::Schema;
use serde::{Deserialize, Serialize};

/// Root configuration for the demo application.
///
/// Contains top-level scalar keys and two nested sub-configs to demonstrate
/// clapfig's hierarchical merge across files, env vars, and CLI flags.
#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct DemoConfig {
    /// Application name shown in the echo banner.
    #[clapfig(default = "clapfig-demo")]
    pub name: String,

    /// Enable verbose output.
    #[clapfig(default = false)]
    pub verbose: bool,

    /// Server settings (nested config).
    pub server: ServerConfig,

    /// Display and formatting settings (nested config).
    pub display: DisplayConfig,
}

/// Server-related configuration.
///
/// Lives under the `[server]` section in TOML files and is accessed via
/// `server.*` dotted keys.
#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct ServerConfig {
    /// Hostname to bind to.
    #[clapfig(default = "127.0.0.1")]
    pub host: String,

    /// Port number.
    #[clapfig(default = 3000)]
    pub port: u16,

    /// Maximum number of allowed connections.
    #[clapfig(default = 100)]
    pub max_connections: u32,
}

/// Terminal color for the echo command output.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl Color {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Blue => "blue",
            Self::Magenta => "magenta",
            Self::Cyan => "cyan",
            Self::White => "white",
        }
    }
}

/// Output layout for the echo command.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Pretty,
    Plain,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pretty => "pretty",
            Self::Plain => "plain",
        }
    }
}

/// Display and output formatting configuration.
///
/// Lives under the `[display]` section in TOML files. The `color` key is
/// used by the `echo` command to colorize terminal output via ANSI codes.
#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct DisplayConfig {
    /// Terminal color for the echo command output.
    #[clapfig(default = "yellow")]
    pub color: Color,

    /// Output format (pretty or plain).
    #[clapfig(default = "pretty")]
    pub format: OutputFormat,
}
