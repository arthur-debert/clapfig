use std::path::PathBuf;

/// Where to search for config files.
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
}

/// A config operation, independent of any CLI framework.
/// The CLI layer converts parsed clap args into this.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    Gen { output: Option<PathBuf> },
    Get { key: String },
    Set { key: String, value: String },
}
