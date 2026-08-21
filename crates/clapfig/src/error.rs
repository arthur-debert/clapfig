//! Error types for clapfig operations.
//!
//! All errors are designed to be shown directly to end users. Each variant
//! includes enough context to diagnose the problem without reaching for a
//! debugger: file paths and, when the span index locates the key, line
//! numbers (TOML, YAML, and JSON) for unknown keys, the list of
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
//! Errors from the underlying format parsers are wrapped (as the format
//! module's [`FormatError`](crate::format::FormatError)) rather than
//! re-invented, so you still get their full detail.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

use crate::format::Span;
use crate::types::InputType;

/// A single unknown-key violation discovered during strict-mode validation.
///
/// Flat, pattern-match-free data carrier: no nested enums to unwrap. Produced
/// during strict-mode validation and surfaced through
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
    /// Path to the config file that contained the unknown key. Non-file
    /// winners use a synthesized placeholder (`<env>`, `<url>`,
    /// `<override>`) — renderers name the variable, query key, or
    /// override key instead of dressing those sources as a config file.
    pub path: PathBuf,
    /// 1-indexed line number where the key was found, or `0` if the line
    /// could not be located. Derived from [`span`](Self::span) (the key
    /// token) when the file's span index has an entry; `0` when the
    /// index has no entry for the path or the origin is not a file.
    /// Renderers suppress the line entirely rather than print a bogus
    /// `line 0`.
    pub line: usize,
    /// Full contents of the config file, shared across all infos from the
    /// same file. Used by renderers for source snippets. `None` for
    /// env-derived keys.
    pub source: Option<Arc<str>>,
    /// For a key derived from the environment layer: the environment
    /// variable that supplied it (e.g. `MYAPP__ROGUE_KEY`). Renderers use
    /// this to describe the error as an env problem — naming the variable
    /// to unset — instead of dressing it in config-file clothing.
    pub env_var: Option<String>,
    /// Byte span of the **key** token in `source` (ADR-0006). Set from
    /// the file's span index when that path has a key token; `None` when
    /// the index has no entry or the origin is not a file.
    pub span: Option<Span>,
    /// URL query-parameter key that supplied this unknown key, when it
    /// came from the URL layer.
    pub url_key: Option<String>,
    /// Override key that supplied this unknown key, when it came from a
    /// programmatic override (`cli_override` / `cli_overrides_from`).
    pub override_key: Option<String>,
    /// Which input type produced the key. `None` when unset; env-derived
    /// keys already name [`env_var`](Self::env_var).
    pub input_type: Option<InputType>,
}

/// Outcome of one discovery probe of a candidate config file.
///
/// Under [`SearchMode::FirstMatch`](crate::SearchMode::FirstMatch),
/// candidates the search never reached are [`NotProbed`](Self::NotProbed),
/// never [`Missing`](Self::Missing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The file existed and was loaded.
    Loaded,
    /// The file was probed and does not exist.
    Missing,
    /// Probe-time failure on an injected record (`error: …` in
    /// [`ClapfigError::MissingRequired`] rendering).
    ///
    /// Production discovery does not produce this variant: a
    /// non-NotFound read error aborts the whole resolve as
    /// [`ClapfigError::IoError`] (an unreadable config file is not a
    /// missing-key search). Parse failures after a successful read are
    /// [`ClapfigError::ParseError`] on a [`Loaded`](Self::Loaded) probe.
    Error {
        /// Human-readable failure detail.
        message: String,
    },
    /// The search never reached this candidate (FirstMatch stopped
    /// earlier).
    NotProbed,
}

impl fmt::Display for ProbeOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loaded => f.write_str("loaded"),
            Self::Missing => f.write_str("missing"),
            Self::Error { message } => write!(f, "error: {message}"),
            Self::NotProbed => f.write_str("not probed"),
        }
    }
}

/// One discovery probe of a candidate config file, with its outcome.
///
/// The record names paths actually probed (stem-mode included), not the
/// format registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProbe {
    /// Path that discovery considered.
    pub path: PathBuf,
    /// What happened at that path.
    pub outcome: ProbeOutcome,
}

/// Files discovery looked at, plus which non-file input types were
/// consulted — the facts [`ClapfigError::MissingRequired`] reports
/// instead of a winning origin (an absent key has none).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryRecord {
    /// Candidate file probes, in search order, with outcomes.
    /// Empty when `Layer::Files` was omitted from `layer_order` (the
    /// files layer was not consulted).
    pub files: Vec<FileProbe>,
    /// Whether the environment layer was consulted (`false` when
    /// `.no_env()` or `Layer::Env` is omitted from `layer_order`).
    pub env: bool,
    /// Whether the URL query layer was consulted (`false` when no query
    /// was supplied or `Layer::Url` is omitted).
    pub url: bool,
    /// Whether programmatic overrides were consulted (`false` when none
    /// were supplied or `Layer::Cli` is omitted).
    pub overrides: bool,
}

impl DiscoveryRecord {
    /// Empty record: no probes, no non-file layers consulted.
    ///
    /// Production resolution fills this from the real search; the
    /// synthetic resolve path injects it so tests do not touch the
    /// filesystem.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Origin facts of a value that exists: file, span, env var, URL query
/// key, input type.
///
/// Flattened onto [`UnknownKeyInfo`] / [`UnknownKeyContext`] /
/// [`CollectedUnknown`] (those types already carried some of these).
/// Boxed on [`ClapfigError::InvalidValue`] so the error enum stays
/// small (`clippy::result_large_err`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginFacts {
    /// File that supplied the value, when it came from a file.
    pub file: Option<PathBuf>,
    /// Byte span of the assigned **value** in that file (ADR-0006).
    pub span: Option<Span>,
    /// Full file text, shared across origins from the same parse.
    /// Renderers use it with [`span`](Self::span) to draw a caret over
    /// the assigned value.
    pub source: Option<Arc<str>>,
    /// Environment variable that supplied the value.
    pub env_var: Option<String>,
    /// URL query-parameter key that supplied the value.
    pub url_key: Option<String>,
    /// Override key (`InputType::Override`) or schema key
    /// (`InputType::Default`).
    pub key: Option<String>,
    /// Which input type produced the value.
    pub input_type: Option<InputType>,
}

impl UnknownKeyInfo {
    /// Returns the leaf segment of the dotted key (e.g. `"typo"` for
    /// `"database.typo"`). Used by renderers to highlight the offending token.
    pub fn leaf(&self) -> &str {
        self.key.rsplit('.').next().unwrap_or(&self.key)
    }
}

/// All clapfig operation errors.
///
/// Marked `#[non_exhaustive]`: future variants may be added without a
/// breaking-change major bump. Downstream `match` over `ClapfigError`
/// must include a `_ => ...` arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClapfigError {
    /// One or more unknown keys were found in config files during strict-mode
    /// validation. The vector is never empty.
    #[error("{}", format_unknown_keys(.0))]
    UnknownKeys(Vec<UnknownKeyInfo>),

    /// A format adapter failed to parse a config file. `source_text`
    /// holds the file contents (when retained) so renderers can draw a
    /// snippet. The adapter error is boxed to keep the enum variant small.
    #[error("Failed to parse {}: {source}", path.display())]
    ParseError {
        path: PathBuf,
        source: Box<crate::format::FormatError>,
        source_text: Option<Arc<str>>,
    },

    #[error("Failed to read {}: {source}", path.display())]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A dotted key does not resolve against the config schema (or, for
    /// scoped reads, the scope's document). `suggestion` carries the
    /// nearest valid schema key when one is plausibly a typo away —
    /// compute it with [`crate::meta::nearest_key`].
    #[error("Key not found: {key}{}", suggestion.as_deref().map(|s| format!(" — did you mean '{s}'?")).unwrap_or_default())]
    KeyNotFound {
        key: String,
        suggestion: Option<String>,
    },

    /// A `config set` action key targets a path a dotted CLI key cannot
    /// address as given: a [`Shape::Array`](crate::runtime::Shape::Array) /
    /// [`Shape::Map`](crate::runtime::Shape::Map) of objects (or a root
    /// map) or a path inside one — entry keys and array indexes are user
    /// data, not schema fields — or a variant-specific / structurally
    /// conflicting field of a [`Shape::Tagged`](crate::runtime::Shape::Tagged)
    /// union (no valid discriminator selects a variant, or the selected
    /// variant does not declare the key). Array/map interiors are edited
    /// in the config file. Tagged-union keys need a valid discriminator
    /// (set it first) or a file edit. (An indexed path syntax like
    /// `servers[0].host` is a possible future extension.)
    #[error("{}", format_unaddressable_key(.key, .section, .kind))]
    UnaddressableKey {
        /// The dotted action key as the caller supplied it.
        key: String,
        /// Canonical dotted path of the array, map-of-objects, or tagged
        /// union the key runs into.
        section: String,
        /// `"an array"`, `"a map"`, or `"a tagged union"` — which kind
        /// refused.
        kind: &'static str,
    },

    #[error("{}", format_invalid_value(.key, .reason, .origin))]
    InvalidValue {
        key: String,
        reason: String,
        /// Origin of the winning value (file, span, env var, URL key,
        /// input type). Boxed so [`ClapfigError`] stays a small `Result`
        /// error. [`ClapfigError::MissingRequired`] does not carry this —
        /// an absent key has no origin, it has a [`DiscoveryRecord`].
        origin: Box<OriginFacts>,
    },

    #[error("No persist scopes configured — call .persist_scope() on the builder")]
    NoPersistPath,

    #[error("Ancestors is not valid as a persist scope path (it resolves to multiple directories)")]
    AncestorsNotAllowedAsPersistPath,

    #[error("Unknown scope '{scope}' — available scopes: {}", available.join(", "))]
    UnknownScope {
        scope: String,
        available: Vec<String>,
    },

    /// A scoped read (`config get --scope <name>`) targeted a scope whose
    /// config file does not exist yet. Distinct from
    /// [`KeyNotFound`](Self::KeyNotFound): the key may be perfectly valid
    /// — there is simply no file to read it from.
    #[error("Scope '{scope}' has no config file: {} does not exist", path.display())]
    ScopeFileMissing { scope: String, path: PathBuf },

    #[error("Unknown config subcommand: '{0}'")]
    UnknownSubcommand(String),

    #[error("App name is required — call .app_name() on the builder")]
    AppNameRequired,

    /// A user-supplied `post_validate` hook rejected the merged configuration.
    ///
    /// The inner string is the message returned by the hook — typically
    /// something like `"port 80 is below the allowed minimum 1024"`. Clapfig
    /// does not interpret it; the displayed/rendered form includes the
    /// `"Configuration validation failed: "` prefix plus the hook's message.
    #[error("Configuration validation failed: {0}")]
    PostValidationFailed(String),

    /// With `.normalize_keys(true)` enabled, two distinct keys in the same
    /// table collapse to the same normalized name (e.g. `pool-size` and
    /// `pool_size` both become `pool_size`). Surfacing this as an error
    /// avoids the silent-drop ambiguity where one entry would win based on
    /// the table's key iteration order. Raised at load (whole-table check)
    /// and whenever `config set`/`unset`/scoped `get` traverses an
    /// ambiguous table — equivalent spellings never silently compete on
    /// any path. Fix by keeping only one spelling.
    #[error(
        "Conflicting keys in {}: '{normalized_key}'{} is defined by [{}], which normalize to the same name",
        path.display(),
        if section.is_empty() { String::new() } else { format!(" (under [{section}])") },
        originals.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "),
    )]
    NormalizedKeyCollision {
        path: PathBuf,
        section: String,
        normalized_key: String,
        originals: Vec<String>,
    },

    /// A required field declared by the [`Schema`](crate::runtime::Schema)
    /// was not supplied by any layer and has no default.
    ///
    /// An absent key has no winning origin — including a nested leaf
    /// whose parent map exists from some input type. The message lists
    /// file probes and consulted non-file input types, never an origin
    /// line.
    #[error("{}", format_missing_required(.key, .discovery))]
    MissingRequired {
        key: String,
        /// Files probed (with outcomes) and non-file input types
        /// consulted. An absent key has no winning origin — this is the
        /// search, not an origin.
        discovery: DiscoveryRecord,
    },

    /// A `strict_at` builder override targets a path that does not resolve
    /// to a nested-section node in the config schema. Either the path does
    /// not exist at all, or it targets a leaf (strict is a container
    /// property, not a per-leaf one).
    #[error("Invalid strict_at path '{path}': {reason}")]
    InvalidStrictPath { path: String, reason: String },

    /// A format adapter refused or failed an operation outside the
    /// file-parse path (template generation, serialization, an edit) —
    /// including the typed
    /// [`UnsupportedByFormat`](crate::format::UnsupportedByFormat)
    /// capability refusal.
    #[error(transparent)]
    Format(#[from] crate::format::FormatError),

    /// A format name or file extension resolves to no shipped adapter:
    /// the builder's `formats(...)` list names an unknown format, a
    /// file name / explicit path (a persist target, `gen --output`)
    /// carries an extension no adapter claims, or a file reaching the
    /// resolve pipeline carries an extension no enabled adapter claims.
    /// Extensionless names are not this error — they fall back to TOML
    /// (exact-name discovery, explicit paths) or the preferred format
    /// (`gen --output`, pipeline parsing).
    #[error("Unknown format '{name}' — available formats: {}", available.join(", "))]
    UnknownFormat {
        name: String,
        available: Vec<String>,
    },

    /// The builder's `formats(...)` list cannot form a usable registry:
    /// it is empty (no preferred format exists for `config gen` or file
    /// seeding) or repeats a format name (stem discovery would collect
    /// the same file twice and misreport it as ambiguous).
    #[error("Invalid formats list: {reason}")]
    InvalidFormats { reason: String },

    /// Stem-based discovery found more than one same-stem config file in
    /// one directory (e.g. `myapp.toml` AND `myapp.yaml`). The spec pins
    /// this as a hard error naming the files — no silent precedence, no
    /// merging of same-stem siblings.
    #[error("Ambiguous config files in {}: {} — keep exactly one of them", dir.display(), files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    AmbiguousConfigFiles { dir: PathBuf, files: Vec<PathBuf> },
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

    /// If this error is a config-file parse failure, return the file
    /// path, the underlying adapter error, and the source text (when
    /// retained).
    pub fn parse_error(&self) -> Option<(&Path, &crate::format::FormatError, Option<&str>)> {
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

    /// Schema/type-validation error with origin facts left unset.
    ///
    /// Persist-path and other non-merge checks have no origin tree;
    /// post-merge `finalize` uses [`Self::invalid_value_at`].
    pub(crate) fn invalid_value(key: impl Into<String>, reason: impl Into<String>) -> Self {
        ClapfigError::InvalidValue {
            key: key.into(),
            reason: reason.into(),
            origin: Box::new(OriginFacts::default()),
        }
    }

    /// Schema/type-validation error naming the winning origin at `path`.
    pub(crate) fn invalid_value_at(
        key: impl Into<String>,
        reason: impl Into<String>,
        origins: &crate::origin::OriginMap,
        path: &crate::format::ConfigPath,
    ) -> Self {
        let facts = crate::origin::lookup(origins, path)
            .map(crate::origin::Origin::to_facts)
            .unwrap_or_default();
        ClapfigError::InvalidValue {
            key: key.into(),
            reason: reason.into(),
            origin: Box::new(facts),
        }
    }

    /// Required-key absence carrying the discovery record of the search
    /// that did not find `key`.
    pub(crate) fn missing_required(key: impl Into<String>, discovery: DiscoveryRecord) -> Self {
        ClapfigError::MissingRequired {
            key: key.into(),
            discovery,
        }
    }
}

fn format_unaddressable_key(key: &str, section: &str, kind: &str) -> String {
    // Tagged-union keys are addressable after a discriminator is set;
    // array/map interiors are not.
    if kind == "a tagged union" {
        format!(
            "Key '{key}' cannot be set: '{section}' is {kind} of sections, and this variant-specific key needs a valid discriminator — set it first, or edit the config file directly"
        )
    } else {
        format!(
            "Key '{key}' cannot be set: '{section}' is {kind} of sections, and keys inside it cannot be addressed with a dotted CLI key — edit the config file directly"
        )
    }
}

fn format_invalid_value(key: &str, reason: &str, origin: &OriginFacts) -> String {
    use std::fmt::Write;
    let mut out = format!("Invalid value for '{key}': {reason}");
    match origin.input_type {
        Some(InputType::File) => {
            if let Some(file) = &origin.file {
                if let (Some(span), Some(src)) = (origin.span, origin.source.as_deref()) {
                    let (line, _) = crate::format::byte_offset_to_line_col(src, span.start);
                    let _ = write!(out, "\n  --> {}:{line}", file.display());
                } else {
                    let _ = write!(out, "\n  --> {}", file.display());
                }
            }
        }
        Some(InputType::Env) => {
            if let Some(var) = &origin.env_var {
                let _ = write!(out, "\n  set by environment variable {var}");
            }
        }
        Some(InputType::Url) => {
            if let Some(url_key) = &origin.url_key {
                let _ = write!(out, "\n  set by URL query parameter {url_key}");
            }
        }
        Some(InputType::Override) => {
            let override_key = origin.key.as_deref().unwrap_or(key);
            let _ = write!(
                out,
                "\n  set by a programmatic override for key {override_key}"
            );
        }
        Some(InputType::Default) => {
            let schema_key = origin.key.as_deref().unwrap_or(key);
            let _ = write!(out, "\n  set by schema default for key {schema_key}");
        }
        None => {}
    }
    out
}

fn format_missing_required(key: &str, discovery: &DiscoveryRecord) -> String {
    use std::fmt::Write;
    let mut out = format!("Missing required key: {key}");
    for probe in &discovery.files {
        let _ = write!(out, "\n  {} ({})", probe.path.display(), probe.outcome);
    }
    let mut consulted = Vec::new();
    if discovery.env {
        consulted.push("env");
    }
    if discovery.url {
        consulted.push("url");
    }
    if discovery.overrides {
        consulted.push("overrides");
    }
    if !consulted.is_empty() {
        let _ = write!(out, "\n  consulted: {}", consulted.join(", "));
    }
    out
}

fn format_unknown_keys(infos: &[UnknownKeyInfo]) -> String {
    use std::fmt::Write;
    let header = unknown_keys_header(infos);
    let mut out = String::from(header);
    for info in infos {
        if let Some(var) = &info.env_var {
            let _ = write!(out, "\n  - '{}' from environment variable {var}", info.key);
        } else if let Some(url_key) = &info.url_key {
            let _ = write!(
                out,
                "\n  - '{}' from URL query parameter {url_key}",
                info.key
            );
        } else if let Some(override_key) = &info.override_key {
            let _ = write!(
                out,
                "\n  - '{}' from programmatic override {override_key}",
                info.key
            );
        } else if info.line == 0 {
            // Line 0 means "could not be located" (no span-index entry
            // for the path) — omit it rather than render a bogus
            // `(line 0)`.
            let _ = write!(out, "\n  - '{}' in {}", info.key, info.path.display());
        } else {
            let _ = write!(
                out,
                "\n  - '{}' in {} (line {})",
                info.key,
                info.path.display(),
                info.line,
            );
        }
    }
    out
}

fn unknown_keys_header(infos: &[UnknownKeyInfo]) -> &'static str {
    if infos.iter().all(|i| i.env_var.is_some()) {
        "Unknown keys in environment:"
    } else if infos.iter().all(|i| i.url_key.is_some()) {
        "Unknown keys in URL query:"
    } else if infos.iter().all(|i| i.override_key.is_some()) {
        "Unknown keys in programmatic overrides:"
    } else if infos
        .iter()
        .all(|i| i.env_var.is_none() && i.url_key.is_none() && i.override_key.is_none())
    {
        "Unknown keys in config file:"
    } else {
        "Unknown keys:"
    }
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
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }
    }

    fn key_not_found(key: &str) -> ClapfigError {
        ClapfigError::KeyNotFound {
            key: key.into(),
            suggestion: None,
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
        assert!(key_not_found("x").unknown_keys().is_none());
    }

    #[test]
    fn is_strict_violation_matches_only_unknown_keys() {
        assert!(ClapfigError::UnknownKeys(vec![info("x", 1)]).is_strict_violation());
        assert!(!key_not_found("x").is_strict_violation());
        assert!(!ClapfigError::AppNameRequired.is_strict_violation());
    }

    #[test]
    fn key_not_found_formats() {
        let err = key_not_found("database.url");
        let msg = err.to_string();
        assert!(msg.contains("database.url"));
        assert!(!msg.contains("did you mean"), "{msg}");
    }

    #[test]
    fn key_not_found_formats_suggestion() {
        let err = ClapfigError::KeyNotFound {
            key: "database.ur1".into(),
            suggestion: Some("database.url".into()),
        };
        let msg = err.to_string();
        assert!(msg.contains("did you mean 'database.url'?"), "{msg}");
    }

    #[test]
    fn unaddressable_tagged_union_points_at_discriminator() {
        let err = ClapfigError::UnaddressableKey {
            key: "block.artifact".into(),
            section: "block".into(),
            kind: "a tagged union",
        };
        let msg = err.to_string();
        assert!(
            msg.contains("this variant-specific key needs a valid discriminator"),
            "{msg}"
        );
        assert!(msg.contains("set it first"), "{msg}");
        assert!(
            !msg.contains("cannot be addressed with a dotted CLI key"),
            "{msg}"
        );
    }

    #[test]
    fn unaddressable_map_stays_file_only() {
        let err = ClapfigError::UnaddressableKey {
            key: "servers.web.host".into(),
            section: "servers".into(),
            kind: "a map",
        };
        let msg = err.to_string();
        assert!(
            msg.contains("keys inside it cannot be addressed with a dotted CLI key"),
            "{msg}"
        );
        assert!(!msg.contains("discriminator"), "{msg}");
    }

    #[test]
    fn unknown_key_line_zero_is_suppressed() {
        // Line 0 means "could not be located"; never print `(line 0)`.
        let msg = ClapfigError::UnknownKeys(vec![info("typo", 0)]).to_string();
        assert!(msg.contains("'typo' in"), "{msg}");
        assert!(!msg.contains("line"), "{msg}");
    }

    #[test]
    fn env_derived_unknown_key_names_the_variable() {
        let mut i = info("rogue_key", 0);
        i.path = "<env>".into();
        i.env_var = Some("MYAPP__ROGUE_KEY".into());
        let msg = ClapfigError::UnknownKeys(vec![i]).to_string();
        assert!(msg.contains("Unknown keys in environment:"), "{msg}");
        assert!(
            msg.contains("'rogue_key' from environment variable MYAPP__ROGUE_KEY"),
            "{msg}"
        );
        assert!(!msg.contains("config file"), "{msg}");
        assert!(!msg.contains("<env>"), "{msg}");
    }

    #[test]
    fn url_derived_unknown_key_names_the_query_parameter() {
        let mut i = info("artifact", 0);
        i.path = "<url>".into();
        i.url_key = Some("artifact".into());
        i.input_type = Some(InputType::Url);
        let msg = ClapfigError::UnknownKeys(vec![i]).to_string();
        assert!(msg.contains("Unknown keys in URL query:"), "{msg}");
        assert!(
            msg.contains("'artifact' from URL query parameter artifact"),
            "{msg}"
        );
        assert!(!msg.contains("<env>"), "{msg}");
        assert!(!msg.contains("config file"), "{msg}");
    }

    #[test]
    fn override_derived_unknown_key_names_the_override_key() {
        let mut i = info("artifact", 0);
        i.path = "<override>".into();
        i.override_key = Some("artifact".into());
        i.input_type = Some(InputType::Override);
        let msg = ClapfigError::UnknownKeys(vec![i]).to_string();
        assert!(
            msg.contains("Unknown keys in programmatic overrides:"),
            "{msg}"
        );
        assert!(
            msg.contains("'artifact' from programmatic override artifact"),
            "{msg}"
        );
        assert!(!msg.contains("<env>"), "{msg}");
        assert!(!msg.contains("config file"), "{msg}");
    }

    #[test]
    fn scope_file_missing_names_scope_and_path() {
        let err = ClapfigError::ScopeFileMissing {
            scope: "local".into(),
            path: "/proj/.myapp.toml".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("'local'"), "{msg}");
        assert!(msg.contains("/proj/.myapp.toml"), "{msg}");
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

    #[test]
    fn discovery_record_constructs_probe_outcomes() {
        let record = DiscoveryRecord {
            files: vec![
                FileProbe {
                    path: "/etc/myapp.toml".into(),
                    outcome: ProbeOutcome::Loaded,
                },
                FileProbe {
                    path: "/home/me/.myapp.toml".into(),
                    outcome: ProbeOutcome::Missing,
                },
                FileProbe {
                    path: "/proj/.myapp.toml".into(),
                    outcome: ProbeOutcome::Error {
                        message: "permission denied".into(),
                    },
                },
                FileProbe {
                    path: "/unreached/.myapp.toml".into(),
                    outcome: ProbeOutcome::NotProbed,
                },
            ],
            env: true,
            url: false,
            overrides: true,
        };
        assert_eq!(record.files.len(), 4);
        assert!(record.env);
        assert!(!record.url);
        assert!(record.overrides);
        assert_eq!(record.files[3].outcome, ProbeOutcome::NotProbed);
    }

    #[test]
    fn missing_required_carries_discovery_not_an_origin() {
        let discovery = DiscoveryRecord {
            files: vec![
                FileProbe {
                    path: "/etc/myapp.toml".into(),
                    outcome: ProbeOutcome::Missing,
                },
                FileProbe {
                    path: "/proj/.myapp.toml".into(),
                    outcome: ProbeOutcome::NotProbed,
                },
            ],
            env: true,
            url: false,
            overrides: true,
        };
        let err = ClapfigError::missing_required("database.url", discovery.clone());
        match err {
            ClapfigError::MissingRequired {
                key,
                discovery: got,
            } => {
                assert_eq!(key, "database.url");
                assert_eq!(got, discovery);
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_display_lists_probes_and_consulted_types_not_an_origin() {
        let err = ClapfigError::missing_required(
            "database.url",
            DiscoveryRecord {
                files: vec![
                    FileProbe {
                        path: "/etc/myapp.toml".into(),
                        outcome: ProbeOutcome::Missing,
                    },
                    FileProbe {
                        path: "/home/me/.myapp.toml".into(),
                        outcome: ProbeOutcome::Loaded,
                    },
                    FileProbe {
                        path: "/proj/.myapp.toml".into(),
                        outcome: ProbeOutcome::NotProbed,
                    },
                ],
                env: true,
                url: false,
                overrides: true,
            },
        );
        let msg = err.to_string();
        assert!(
            msg.starts_with("Missing required key: database.url"),
            "{msg}"
        );
        assert!(msg.contains("/etc/myapp.toml (missing)"), "{msg}");
        assert!(msg.contains("/home/me/.myapp.toml (loaded)"), "{msg}");
        assert!(msg.contains("/proj/.myapp.toml (not probed)"), "{msg}");
        assert!(msg.contains("consulted: env, overrides"), "{msg}");
        assert!(
            !msg.contains("consulted: env, url"),
            "url was not consulted: {msg}"
        );
        assert!(
            !msg.contains("set by"),
            "MissingRequired must not name a winning origin: {msg}"
        );
        assert!(
            !msg.contains("origin"),
            "MissingRequired must not name a winning origin: {msg}"
        );
    }

    #[test]
    fn missing_required_empty_discovery_is_the_key_line_only() {
        let msg = ClapfigError::missing_required("name", DiscoveryRecord::empty()).to_string();
        assert_eq!(msg, "Missing required key: name");
    }

    #[test]
    fn probe_outcome_display_uses_spec_vocabulary() {
        assert_eq!(ProbeOutcome::Loaded.to_string(), "loaded");
        assert_eq!(ProbeOutcome::Missing.to_string(), "missing");
        assert_eq!(ProbeOutcome::NotProbed.to_string(), "not probed");
        assert_eq!(
            ProbeOutcome::Error {
                message: "permission denied".into(),
            }
            .to_string(),
            "error: permission denied"
        );
    }

    #[test]
    fn invalid_value_origin_facts_start_unset() {
        let err = ClapfigError::invalid_value("port", "expected integer");
        match err {
            ClapfigError::InvalidValue {
                key,
                reason,
                origin,
            } => {
                assert_eq!(key, "port");
                assert_eq!(reason, "expected integer");
                assert_eq!(*origin, OriginFacts::default());
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
        assert_eq!(
            ClapfigError::invalid_value("port", "expected integer").to_string(),
            "Invalid value for 'port': expected integer"
        );
    }

    #[test]
    fn invalid_value_display_names_each_origin_kind() {
        let mut file = OriginFacts {
            file: Some("app.toml".into()),
            span: Some(crate::format::Span { start: 7, end: 11 }),
            source: Some(Arc::from("port = 8080\n")),
            input_type: Some(InputType::File),
            ..OriginFacts::default()
        };
        let msg = ClapfigError::InvalidValue {
            key: "port".into(),
            reason: "expected integer".into(),
            origin: Box::new(file.clone()),
        }
        .to_string();
        assert!(msg.contains("--> app.toml:1"), "{msg}");

        file.input_type = Some(InputType::Env);
        file.env_var = Some("MYAPP__PORT".into());
        file.file = None;
        file.span = None;
        file.source = None;
        let msg = ClapfigError::InvalidValue {
            key: "port".into(),
            reason: "expected integer".into(),
            origin: Box::new(file.clone()),
        }
        .to_string();
        assert!(
            msg.contains("set by environment variable MYAPP__PORT"),
            "{msg}"
        );

        let msg = ClapfigError::PostValidationFailed("nope".into()).to_string();
        assert!(!msg.contains("set by"), "{msg}");
        assert!(!msg.contains("-->"), "{msg}");
    }
}
