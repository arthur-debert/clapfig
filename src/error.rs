use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClapfigError {
    #[error("Unknown key '{key}' in {path} (line {line})")]
    UnknownKey {
        key: String,
        path: PathBuf,
        line: usize,
    },

    #[error("Unknown keys in config file")]
    UnknownKeys(Vec<ClapfigError>),

    #[error("Failed to parse {path}: {source}")]
    ParseError {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("Failed to read {path}: {source}")]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Configuration error: {0}")]
    ConfigError(#[from] confique::Error),

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Invalid value for '{key}': {reason}")]
    InvalidValue { key: String, reason: String },

    #[error("No persist path configured — call .persist_path() on the builder")]
    NoPersistPath,

    #[error("Ancestors is not valid as a persist path (it resolves to multiple directories)")]
    AncestorsNotAllowedAsPersistPath,

    #[error("App name is required — call .app_name() on the builder")]
    AppNameRequired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_key_formats_correctly() {
        let err = ClapfigError::UnknownKey {
            key: "typo_key".into(),
            path: "/home/user/.config/myapp/config.toml".into(),
            line: 42,
        };
        let msg = err.to_string();
        assert!(msg.contains("typo_key"));
        assert!(msg.contains("config.toml"));
        assert!(msg.contains("42"));
    }

    #[test]
    fn key_not_found_formats() {
        let err = ClapfigError::KeyNotFound("database.url".into());
        assert!(err.to_string().contains("database.url"));
    }

    #[test]
    fn app_name_required_formats() {
        let err = ClapfigError::AppNameRequired;
        assert!(err.to_string().contains("app_name"));
    }
}
