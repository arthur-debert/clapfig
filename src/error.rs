//! Error types for clapfig operations.
//!
//! All errors are designed to be shown directly to end users. Each variant
//! includes enough context to diagnose the problem without reaching for a
//! debugger: file paths and line numbers for unknown keys, the list of
//! available scopes when a scope name is wrong, and references to the
//! builder method that needs to be called when a prerequisite is missing.
//!
//! # Structured data vs. rendering
//!
//! `ClapfigError` is the *data layer*: variants carry the raw facts about
//! what went wrong (unknown key names, file paths, line numbers, parser spans).
//! Accessor methods like [`ClapfigError::unknown_keys`] and
//! [`ClapfigError::parse_error`] expose that data without requiring callers to
//! pattern-match on enum variants.
//!
//! For user-facing output, use the [`crate::render`] module:
//!
//! - [`render_plain`](crate::render::render_plain) — ANSI-free text, safe for
//!   logs and non-TTY targets.
//! - [`render_rich`](crate::render::render_rich) — colored output with source
//!   snippets and carets (requires the `rich-errors` feature).
//!
//! Errors from the underlying TOML parser and from confique's validation are
//! wrapped rather than re-invented, so you still get their full detail.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// A single unknown-key violation discovered during strict-mode validation.
///
/// Flat, pattern-match-free data carrier: no nested enums to unwrap. Produced
/// by [`validate`](crate::error) and surfaced through
/// [`ClapfigError::unknown_keys`].
///
/// `source` holds the full file contents at the time of validation, shared
/// cheaply across all infos from the same file. Renderers use it to draw
/// source snippets; it is `None` when the source is not retained (e.g. after
/// round-tripping through a non-data path).
#[derive(Debug, Clone)]
pub struct UnknownKeyInfo {
    /// Dotted key path that was not recognized by the config schema
    /// (e.g. `"database.typo"`).
    pub key: String,
    /// Path to the config file that contained the unknown key.
    pub path: PathBuf,
    /// 1-indexed line number where the key was found, or `0` if the line
    /// could not be located (best-effort heuristic).
    pub line: usize,
    /// Full contents of the config file, shared across all infos from the
    /// same file. Used by renderers for source snippets.
    pub source: Option<Arc<str>>,
}

impl UnknownKeyInfo {
    /// Returns the leaf segment of the dotted key (e.g. `"typo"` for
    /// `"database.typo"`). Used by renderers to highlight the offending token.
    pub fn leaf(&self) -> &str {
        self.key.rsplit('.').next().unwrap_or(&self.key)
    }
}

#[derive(Debug, Error)]
pub enum ClapfigError {
    /// One or more unknown keys were found in config files during strict-mode
    /// validation. The vector is never empty.
    #[error("{}", format_unknown_keys(.0))]
    UnknownKeys(Vec<UnknownKeyInfo>),

    /// The TOML parser failed on a config file. `source_text` holds the
    /// file contents (when retained) so renderers can draw a snippet.
    /// The parser error is boxed to keep the enum variant small.
    #[error("Failed to parse {}: {source}", path.display())]
    ParseError {
        path: PathBuf,
        source: Box<toml::de::Error>,
        source_text: Option<Arc<str>>,
    },

    #[error("Failed to read {}: {source}", path.display())]
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

    #[error("No persist scopes configured — call .persist_scope() on the builder")]
    NoPersistPath,

    #[error("Ancestors is not valid as a persist scope path (it resolves to multiple directories)")]
    AncestorsNotAllowedAsPersistPath,

    #[error("Unknown scope '{scope}' — available scopes: {}", available.join(", "))]
    UnknownScope {
        scope: String,
        available: Vec<String>,
    },

    #[error("Unknown config subcommand: '{0}'")]
    UnknownSubcommand(String),

    #[error("App name is required — call .app_name() on the builder")]
    AppNameRequired,
}

impl ClapfigError {
    /// If this error carries unknown-key information, return the list.
    ///
    /// Callers that want to render their own error UI can iterate this
    /// directly without pattern-matching on the enum.
    pub fn unknown_keys(&self) -> Option<&[UnknownKeyInfo]> {
        match self {
            ClapfigError::UnknownKeys(infos) => Some(infos),
            _ => None,
        }
    }

    /// If this error is a TOML parse failure, return the file path, the
    /// underlying parser error, and the source text (when retained).
    pub fn parse_error(&self) -> Option<(&Path, &toml::de::Error, Option<&str>)> {
        match self {
            ClapfigError::ParseError {
                path,
                source,
                source_text,
            } => Some((path.as_path(), source.as_ref(), source_text.as_deref())),
            _ => None,
        }
    }

    /// True if this error represents a strict-mode schema violation
    /// (unknown keys) — useful for callers that want to fail softly on
    /// strict violations but hard on real parse/type errors.
    pub fn is_strict_violation(&self) -> bool {
        matches!(self, ClapfigError::UnknownKeys(_))
    }
}

fn format_unknown_keys(infos: &[UnknownKeyInfo]) -> String {
    use std::fmt::Write;
    let mut out = String::from("Unknown keys in config file:");
    for info in infos {
        let _ = write!(
            out,
            "\n  - '{}' in {} (line {})",
            info.key,
            info.path.display(),
            info.line
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(key: &str, line: usize) -> UnknownKeyInfo {
        UnknownKeyInfo {
            key: key.into(),
            path: "/home/user/.config/myapp/config.toml".into(),
            line,
            source: None,
        }
    }

    #[test]
    fn unknown_keys_formats_correctly() {
        let err = ClapfigError::UnknownKeys(vec![info("typo_key", 42)]);
        let msg = err.to_string();
        assert!(msg.contains("typo_key"));
        assert!(msg.contains("config.toml"));
        assert!(msg.contains("42"));
    }

    #[test]
    fn unknown_keys_accessor_returns_data() {
        let err = ClapfigError::UnknownKeys(vec![info("a", 1), info("b.c", 2)]);
        let keys = err.unknown_keys().expect("should be unknown keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key, "a");
        assert_eq!(keys[1].key, "b.c");
        assert_eq!(keys[1].leaf(), "c");
    }

    #[test]
    fn unknown_keys_accessor_none_for_other_variants() {
        assert!(
            ClapfigError::KeyNotFound("x".into())
                .unknown_keys()
                .is_none()
        );
    }

    #[test]
    fn is_strict_violation_matches_only_unknown_keys() {
        assert!(ClapfigError::UnknownKeys(vec![info("x", 1)]).is_strict_violation());
        assert!(!ClapfigError::KeyNotFound("x".into()).is_strict_violation());
        assert!(!ClapfigError::AppNameRequired.is_strict_violation());
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

    #[test]
    fn leaf_returns_last_segment() {
        assert_eq!(info("a.b.c", 0).leaf(), "c");
        assert_eq!(info("toplevel", 0).leaf(), "toplevel");
    }
}
