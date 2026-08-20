//! Crate-private origin payload (ADR-0004).
//!
//! Origins travel as a shadow tree merged in lockstep with the value tree,
//! not as annotations on [`Value`](crate::value::Value) nodes. This module
//! is the payload of that tree: which provenance layer produced a value,
//! and the facts that layer knows. The tree walk, merge, and defaults
//! injection live in later workstreams.
//!
//! [`OriginLayer`] is **not** the public [`Layer`](crate::Layer) enum.
//! `Layer` is merge-order (`Files` / `Env` / `Url` / `Cli`) and must not
//! grow a `Default` variant. `Override` is the programmatic override
//! layer (`cli_override`); clapfig cannot know whether a CLI flag, GUI
//! field, or HTTP header produced the pair.
//!
//! This type stays crate-private. Public errors carry flattened origin
//! facts ([`InputType`](crate::InputType), file, span, env var, URL key)
//! rather than an `Origin` value — there is no public Origin API.
//!
//! Later workstreams construct and merge these values; until then the
//! types exist so the contract can be reviewed and tested in isolation.

#![allow(dead_code)] // constructed by later workstreams; tests cover the shape

use std::path::PathBuf;
use std::sync::Arc;

use crate::format::Span;
use crate::types::InputType;

/// Provenance layer that produced a resolved value.
///
/// Same variants as the public [`InputType`] reported on errors; this
/// alias is the pipeline name and is not re-exported.
pub(crate) type OriginLayer = InputType;

/// Origin of one resolved value: layer plus whatever that layer knows.
///
/// File origins carry path, the **value** byte span (unknown-key lookup
/// uses the span-index **key** span, ADR-0006), and the file's full text
/// (`Arc<str>`, one per parsed file). Non-file origins leave file / span
/// / text as `None` and fill the field that layer owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin {
    /// Which provenance layer produced the value.
    pub layer: OriginLayer,
    /// Config file path, when `layer` is [`InputType::File`].
    pub file: Option<PathBuf>,
    /// Byte span of the assigned value in `source`, when from a file.
    pub span: Option<Span>,
    /// Full file text, shared across origins from the same parse.
    pub source: Option<Arc<str>>,
    /// Original environment variable name(s), when `layer` is
    /// [`InputType::Env`].
    pub env_vars: Vec<String>,
    /// URL query-parameter key as received (dotted, percent-decoded),
    /// when `layer` is [`InputType::Url`].
    pub url_key: Option<String>,
    /// Override key (`InputType::Override`) or schema key
    /// (`InputType::Default`).
    pub key: Option<String>,
}

impl Origin {
    /// A file origin: path, value span, and retained source text.
    pub(crate) fn file(path: PathBuf, span: Span, source: Arc<str>) -> Self {
        Self {
            layer: OriginLayer::File,
            file: Some(path),
            span: Some(span),
            source: Some(source),
            env_vars: Vec::new(),
            url_key: None,
            key: None,
        }
    }

    /// An environment-variable origin, naming the original variable(s).
    pub(crate) fn env(vars: impl Into<Vec<String>>) -> Self {
        Self {
            layer: OriginLayer::Env,
            file: None,
            span: None,
            source: None,
            env_vars: vars.into(),
            url_key: None,
            key: None,
        }
    }

    /// A URL query-parameter origin, naming the key as received.
    pub(crate) fn url(query_key: impl Into<String>) -> Self {
        Self {
            layer: OriginLayer::Url,
            file: None,
            span: None,
            source: None,
            env_vars: Vec::new(),
            url_key: Some(query_key.into()),
            key: None,
        }
    }

    /// A programmatic-override origin (`cli_override`), naming the
    /// override key.
    pub(crate) fn r#override(override_key: impl Into<String>) -> Self {
        Self {
            layer: OriginLayer::Override,
            file: None,
            span: None,
            source: None,
            env_vars: Vec::new(),
            url_key: None,
            key: Some(override_key.into()),
        }
    }

    /// A schema-default origin, naming the schema key that was filled.
    pub(crate) fn default(schema_key: impl Into<String>) -> Self {
        Self {
            layer: OriginLayer::Default,
            file: None,
            span: None,
            source: None,
            env_vars: Vec::new(),
            url_key: None,
            key: Some(schema_key.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn origin_layer_is_not_the_public_layer_enum() {
        // OriginLayer / InputType names File (singular) and Override, and
        // includes Default. Layer is merge-order: Files / Env / Url / Cli.
        assert_eq!(OriginLayer::File, InputType::File);
        assert_eq!(OriginLayer::Override, InputType::Override);
        assert_eq!(OriginLayer::Default, InputType::Default);
        let _ = [
            OriginLayer::File,
            OriginLayer::Env,
            OriginLayer::Url,
            OriginLayer::Override,
            OriginLayer::Default,
        ];
    }

    #[test]
    fn file_origin_carries_path_span_and_source() {
        let source: Arc<str> = Arc::from("port = 8080\n");
        let origin = Origin::file(
            "/tmp/app.toml".into(),
            Span { start: 7, end: 11 },
            Arc::clone(&source),
        );
        assert_eq!(origin.layer, OriginLayer::File);
        assert_eq!(
            origin.file.as_deref(),
            Some(std::path::Path::new("/tmp/app.toml"))
        );
        assert_eq!(origin.span, Some(Span { start: 7, end: 11 }));
        assert_eq!(origin.source.as_deref(), Some("port = 8080\n"));
        assert!(origin.env_vars.is_empty());
        assert!(origin.url_key.is_none());
        assert!(origin.key.is_none());
    }

    #[test]
    fn env_origin_carries_original_variable_names() {
        let origin = Origin::env(vec!["MYAPP__PORT".into(), "MYAPP__PORT_ALIAS".into()]);
        assert_eq!(origin.layer, OriginLayer::Env);
        assert_eq!(origin.env_vars, ["MYAPP__PORT", "MYAPP__PORT_ALIAS"]);
        assert!(origin.file.is_none());
        assert!(origin.span.is_none());
        assert!(origin.source.is_none());
    }

    #[test]
    fn url_origin_carries_the_query_key() {
        let origin = Origin::url("database.url");
        assert_eq!(origin.layer, OriginLayer::Url);
        assert_eq!(origin.url_key.as_deref(), Some("database.url"));
    }

    #[test]
    fn override_origin_carries_the_override_key() {
        let origin = Origin::r#override("port");
        assert_eq!(origin.layer, OriginLayer::Override);
        assert_eq!(origin.key.as_deref(), Some("port"));
    }

    #[test]
    fn default_origin_carries_the_schema_key() {
        let origin = Origin::default("host");
        assert_eq!(origin.layer, OriginLayer::Default);
        assert_eq!(origin.key.as_deref(), Some("host"));
    }
}
