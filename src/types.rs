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

/// Config file format.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Format {
    #[default]
    Toml,
}

/// A config operation, independent of any CLI framework.
/// The CLI layer converts parsed clap args into this.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    Gen { output: Option<PathBuf> },
    Get { key: String },
    Set { key: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_path_variants_construct() {
        let _ = SearchPath::Platform;
        let _ = SearchPath::Home(".myapp");
        let _ = SearchPath::Cwd;
        let _ = SearchPath::Path(PathBuf::from("/etc/myapp"));
    }

    #[test]
    fn format_default_is_toml() {
        assert_eq!(Format::default(), Format::Toml);
    }

    #[test]
    fn config_action_variants() {
        let _ = ConfigAction::Gen { output: None };
        let _ = ConfigAction::Get {
            key: "host".into(),
        };
        let _ = ConfigAction::Set {
            key: "port".into(),
            value: "3000".into(),
        };
    }
}
