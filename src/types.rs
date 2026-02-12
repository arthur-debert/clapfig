//! Core types that define how clapfig discovers, resolves, and persists configuration.
//!
//! Configuration lookup has three orthogonal axes, each controlled independently
//! on the builder:
//!
//! | Axis | Builder method | Controls |
//! |------|---------------|----------|
//! | **Discovery** | [`search_paths()`] | Where to look for config files |
//! | **Resolution** | [`search_mode()`] | Whether to merge all found files or pick one |
//! | **Persistence** | [`persist_path()`] | Where `config set` writes (explicit, no guessing) |
//!
//! [`search_paths()`]: crate::ClapfigBuilder::search_paths
//! [`search_mode()`]: crate::ClapfigBuilder::search_mode
//! [`persist_path()`]: crate::ClapfigBuilder::persist_path
//!
//! # Discovery: [`SearchPath`]
//!
//! A search path is a source of candidate directories. The builder accepts a list
//! of them in **priority-ascending order** — the last entry has the highest priority.
//!
//! Most variants resolve to a single directory (`Platform`, `Home`, `Cwd`, `Path`).
//! The [`Ancestors`](SearchPath::Ancestors) variant is special: it expands inline
//! into multiple directories by walking up from the current working directory,
//! emitting ancestors from shallowest (root) to deepest (CWD) so that deeper
//! directories have higher priority.
//!
//! # Resolution: [`SearchMode`]
//!
//! Controls what happens once candidate directories have been searched:
//!
//! - **[`Merge`](SearchMode::Merge)** (default): All found config files are loaded
//!   and deep-merged. Later (higher-priority) files override earlier ones. This is
//!   the classic layered-config pattern: a global config provides defaults and a
//!   project-local config overrides specific keys.
//!
//! - **[`FirstMatch`](SearchMode::FirstMatch)**: Only the single highest-priority
//!   file found is used. The search starts from the highest-priority end and stops
//!   at the first hit. This is the "find my config" pattern: a tool looks in several
//!   places and uses whichever config it finds first.
//!
//! Both modes use the same priority ordering — you never need to reorder your search
//! paths when switching modes.
//!
//! # Common patterns
//!
//! **Layered global + local** (Merge, default): A CLI app that reads a global config
//! from the platform directory and merges project-local overrides from the working
//! directory. A user sets their preferred theme globally; a project overrides the
//! output format.
//!
//! **Fallback chain** (FirstMatch + explicit paths): A code formatter that looks
//! for a config in the project directory first, then the user's home, then the
//! platform directory. The first config found is used as-is — no merging.
//!
//! **Nearest project config** (FirstMatch + Ancestors): A build tool invoked from
//! a subdirectory walks up the tree to find the nearest project config. Stops at
//! the repository root (`.git` marker).
//!
//! **Per-directory layering** (Merge + Ancestors): An editor plugin that collects
//! style configs from every ancestor directory and merges them, with deeper
//! directories taking precedence — like `.editorconfig`.

use std::path::PathBuf;

/// Where to search for config files.
///
/// Each variant represents a source of candidate directories. The builder
/// accepts a `Vec<SearchPath>` in priority-ascending order: last = highest
/// priority.
///
/// Most variants resolve to a single directory. [`Ancestors`](Self::Ancestors)
/// expands into multiple directories by walking up from the current working
/// directory. See the [module documentation](self) for details.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchPath {
    /// Platform config directory (XDG on Linux, ~/Library/Application Support on macOS).
    Platform,
    /// A subdirectory under the user's home directory, e.g. `Home(".myapp")`.
    Home(&'static str),
    /// Current working directory.
    Cwd,
    /// An explicit absolute path.
    Path(PathBuf),
    /// Walk up from the current working directory, checking each ancestor.
    ///
    /// Expands inline into multiple directories during resolution, ordered from
    /// shallowest (filesystem root) to deepest (CWD). This means CWD has the
    /// highest priority — a config found closer to the working directory wins.
    ///
    /// The [`Boundary`] controls where the walk stops.
    ///
    /// # Note
    ///
    /// This variant is not valid as a [`persist_path`](crate::ClapfigBuilder::persist_path)
    /// because it resolves to multiple directories. Using it there produces an error.
    Ancestors(Boundary),
}

/// Controls where an [`Ancestors`](SearchPath::Ancestors) walk stops.
///
/// The walk always starts at the current working directory and moves toward
/// the filesystem root. The boundary determines where it ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Boundary {
    /// Walk all the way to the filesystem root.
    Root,
    /// Walk until a directory is found that contains the named file or
    /// subdirectory (e.g. `".git"`, `"Cargo.toml"`).
    ///
    /// The directory containing the marker **is included** in the search —
    /// it is typically the project root and a natural place for config files.
    /// If the marker is never found, the walk continues to the filesystem root.
    Marker(&'static str),
}

/// How found config files are resolved into configuration.
///
/// Controls what happens after [`SearchPath`] entries have been expanded into
/// directories and checked for config files. Both modes use the same
/// priority-ascending ordering — the search path list does not need to change
/// when switching modes.
///
/// See the [module-level documentation](self) for use-case examples.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SearchMode {
    /// Merge all found config files, with later (higher-priority) files
    /// overriding earlier ones via deep merge.
    ///
    /// This is the default. Use it when configs are sparse overlays
    /// (e.g. a project config overrides only `output_format` while the global
    /// config provides the rest).
    #[default]
    Merge,
    /// Use only the single highest-priority config file found.
    ///
    /// Searches from the highest-priority end of the list and stops at the
    /// first file that exists. Use it when configs are self-contained and
    /// should not be layered (e.g. a code formatter whose project config
    /// replaces the global one entirely).
    FirstMatch,
}

/// A config operation, independent of any CLI framework.
/// The CLI layer converts parsed clap args into this.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    /// Show all resolved configuration key-value pairs.
    List,
    Gen {
        output: Option<PathBuf>,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
}
