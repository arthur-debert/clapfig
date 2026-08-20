//! Crate-private origin payload and shadow tree (ADR-0004).
//!
//! Origins travel as a shadow tree merged in lockstep with the value tree,
//! not as annotations on [`Value`](crate::value::Value) nodes. This module
//! is the payload of that tree: which provenance layer produced a value,
//! the facts that layer knows, and the tree walk that file parse, env,
//! URL, overrides, and defaults injection all write.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::env::EnvWinners;
use crate::error::OriginFacts;
use crate::format::{ConfigPath, PathSegment, Span, SpanEntry};
use crate::types::InputType;
use crate::value::{Map, Value};

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
    /// Winning original environment variable name, when `layer` is
    /// [`InputType::Env`]. A vec so a node can still carry aliases if a
    /// caller has them; the env walk records one last-writer name.
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
        Self::file_with_span(path, Some(span), source)
    }

    /// A file origin whose span-index lookup missed this path.
    ///
    /// Still a file origin (path + source); renderers omit the line
    /// rather than inventing one.
    pub(crate) fn file_with_span(path: PathBuf, span: Option<Span>, source: Arc<str>) -> Self {
        Self {
            layer: OriginLayer::File,
            file: Some(path),
            span,
            source: Some(source),
            env_vars: Vec::new(),
            url_key: None,
            key: None,
        }
    }

    /// An environment-variable origin, naming the winning original variable.
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
    #[cfg_attr(not(feature = "url"), allow(dead_code))]
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

    /// Flatten onto the public [`OriginFacts`] carried by `InvalidValue`.
    pub(crate) fn to_facts(&self) -> OriginFacts {
        OriginFacts {
            file: self.file.clone(),
            span: self.span,
            source: self.source.clone(),
            env_var: if self.env_vars.is_empty() {
                None
            } else {
                Some(self.env_vars.join(", "))
            },
            url_key: self.url_key.clone(),
            key: self.key.clone(),
            input_type: Some(self.layer),
        }
    }
}

/// Shadow of one [`Value`] node: the origin of this node plus, for
/// maps and arrays, child origins (ADR-0004).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OriginNode {
    pub origin: Origin,
    pub children: OriginChildren,
}

/// Child shape of an [`OriginNode`], matching [`Value::Map`] /
/// [`Value::Array`] / scalar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OriginChildren {
    None,
    Array(Vec<OriginNode>),
    Map(OriginMap),
}

/// Shadow of a value [`Map`]: the same keys, each holding an
/// [`OriginNode`].
pub(crate) type OriginMap = BTreeMap<String, OriginNode>;

impl OriginNode {
    pub(crate) fn leaf(origin: Origin) -> Self {
        Self {
            origin,
            children: OriginChildren::None,
        }
    }

    pub(crate) fn map(origin: Origin, entries: OriginMap) -> Self {
        Self {
            origin,
            children: OriginChildren::Map(entries),
        }
    }

    pub(crate) fn array(origin: Origin, items: Vec<OriginNode>) -> Self {
        Self {
            origin,
            children: OriginChildren::Array(items),
        }
    }

    /// Attach one origin to every node of a value subtree (env / URL /
    /// override inserts that share a single source key).
    pub(crate) fn from_value(value: &Value, origin: Origin) -> Self {
        match value {
            Value::Map(m) => Self::map(
                origin.clone(),
                m.iter()
                    .map(|(k, v)| (k.clone(), Self::from_value(v, origin.clone())))
                    .collect(),
            ),
            Value::Array(a) => Self::array(
                origin.clone(),
                a.iter()
                    .map(|v| Self::from_value(v, origin.clone()))
                    .collect(),
            ),
            _ => Self::leaf(origin),
        }
    }

    pub(crate) fn map_children_mut(&mut self) -> &mut OriginMap {
        if !matches!(self.children, OriginChildren::Map(_)) {
            self.children = OriginChildren::Map(OriginMap::new());
        }
        match &mut self.children {
            OriginChildren::Map(m) => m,
            _ => unreachable!("just set Map children"),
        }
    }

    pub(crate) fn array_children_mut(&mut self) -> &mut Vec<OriginNode> {
        if !matches!(self.children, OriginChildren::Array(_)) {
            self.children = OriginChildren::Array(Vec::new());
        }
        match &mut self.children {
            OriginChildren::Array(a) => a,
            _ => unreachable!("just set Array children"),
        }
    }
}

/// Look up the origin of the node at `path` in a root origin map.
pub(crate) fn lookup<'a>(map: &'a OriginMap, path: &ConfigPath) -> Option<&'a Origin> {
    let mut segments = path.segments().iter();
    let PathSegment::Key(first) = segments.next()? else {
        return None;
    };
    let mut node = map.get(first)?;
    for segment in segments {
        node = match (segment, &node.children) {
            (PathSegment::Key(k), OriginChildren::Map(m)) => m.get(k)?,
            (PathSegment::Index(i), OriginChildren::Array(items)) => items.get(*i)?,
            _ => return None,
        };
    }
    Some(&node.origin)
}

/// Build an origin map from a parsed file table and its span index.
///
/// Each node's origin keeps the **value** span (ADR-0006). Missing
/// index entries still produce a file origin (path + source, no span).
pub(crate) fn origin_map_from_file(
    table: &Map,
    spans: &BTreeMap<ConfigPath, SpanEntry>,
    file: &Path,
    source: &Arc<str>,
) -> OriginMap {
    walk_file_map(table, ConfigPath::new(), spans, file, source)
}

fn walk_file_map(
    table: &Map,
    parent: ConfigPath,
    spans: &BTreeMap<ConfigPath, SpanEntry>,
    file: &Path,
    source: &Arc<str>,
) -> OriginMap {
    table
        .iter()
        .map(|(key, value)| {
            let path = parent.clone().key(key);
            (
                key.clone(),
                walk_file_value(value, path, spans, file, source),
            )
        })
        .collect()
}

fn walk_file_value(
    value: &Value,
    path: ConfigPath,
    spans: &BTreeMap<ConfigPath, SpanEntry>,
    file: &Path,
    source: &Arc<str>,
) -> OriginNode {
    let origin = match spans.get(&path) {
        Some(entry) => Origin::file(file.to_path_buf(), entry.value, Arc::clone(source)),
        None => Origin::file_with_span(file.to_path_buf(), None, Arc::clone(source)),
    };
    match value {
        Value::Map(m) => OriginNode::map(origin, walk_file_map(m, path, spans, file, source)),
        Value::Array(items) => OriginNode::array(
            origin,
            items
                .iter()
                .enumerate()
                .map(|(i, item)| walk_file_value(item, path.clone().index(i), spans, file, source))
                .collect(),
        ),
        _ => OriginNode::leaf(origin),
    }
}

/// Build an origin map from an env-derived table and the last-writer
/// variable names recorded in lockstep with insertion.
///
/// Each node's origin names only the variable that produced the winning
/// value at that path (ADR-0004 winner-only). Env source aggregation
/// stays on unknown-key errors.
pub(crate) fn origin_map_from_env(table: &Map, winners: &EnvWinners) -> OriginMap {
    walk_env_map(table, "", winners)
}

fn walk_env_map(table: &Map, prefix: &str, winners: &EnvWinners) -> OriginMap {
    table
        .iter()
        .map(|(key, value)| {
            let dotted = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            (key.clone(), walk_env_value(value, &dotted, winners))
        })
        .collect()
}

fn walk_env_value(value: &Value, dotted: &str, winners: &EnvWinners) -> OriginNode {
    let origin = Origin::env(
        winners
            .get(dotted)
            .cloned()
            .map(|name| vec![name])
            .unwrap_or_default(),
    );
    match value {
        Value::Map(m) => OriginNode::map(origin, walk_env_map(m, dotted, winners)),
        Value::Array(items) => OriginNode::array(
            origin.clone(),
            items
                .iter()
                .map(|item| OriginNode::from_value(item, origin.clone()))
                .collect(),
        ),
        _ => OriginNode::leaf(origin),
    }
}

/// Split an owned [`OriginNode`] into a map-node origin and its child
/// map, for nested lockstep merge. Non-map shapes yield an empty child
/// map (the caller is already merging two value maps).
pub(crate) fn take_map_children(node: Option<OriginNode>) -> (Option<Origin>, OriginMap) {
    match node {
        Some(OriginNode {
            origin,
            children: OriginChildren::Map(m),
        }) => (Some(origin), m),
        Some(OriginNode { origin, .. }) => (Some(origin), OriginMap::new()),
        None => (None, OriginMap::new()),
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

    #[test]
    fn lookup_distinguishes_quoted_dotted_key_from_nested_path() {
        let quoted = Origin::r#override("quoted");
        let nested = Origin::r#override("nested");
        let mut map = OriginMap::new();
        map.insert("a.b".into(), OriginNode::leaf(quoted.clone()));
        let mut inner = OriginMap::new();
        inner.insert("b".into(), OriginNode::leaf(nested.clone()));
        map.insert("a".into(), OriginNode::map(Origin::r#override("a"), inner));
        assert_eq!(
            lookup(&map, &ConfigPath::new().key("a.b")).map(|o| o.key.as_deref()),
            Some(Some("quoted"))
        );
        assert_eq!(
            lookup(&map, &ConfigPath::new().key("a").key("b")).map(|o| o.key.as_deref()),
            Some(Some("nested"))
        );
    }

    #[test]
    fn to_facts_flattens_env_names_and_input_type() {
        let facts = Origin::env(vec!["MYAPP__PORT".into(), "MYAPP__port".into()]).to_facts();
        assert_eq!(facts.input_type, Some(InputType::Env));
        assert_eq!(facts.env_var.as_deref(), Some("MYAPP__PORT, MYAPP__port"));
        assert!(facts.file.is_none());
        assert!(facts.key.is_none());
    }

    fn env_origin_vars(origins: &OriginMap, dotted: &str) -> Vec<String> {
        let mut path = ConfigPath::new();
        for seg in dotted.split('.') {
            path = path.key(seg);
        }
        lookup(origins, &path)
            .map(|o| o.env_vars.clone())
            .unwrap_or_default()
    }

    #[test]
    fn env_origin_map_names_only_the_winning_variable() {
        let (table, _, winners) = crate::env::env_to_table_with_sources(
            "APP",
            [
                ("APP__DATABASE__URL".into(), "x".into()),
                ("APP__DATABASE".into(), "oops".into()),
            ],
        );
        let origins = origin_map_from_env(&table, &winners);
        assert_eq!(env_origin_vars(&origins, "database"), ["APP__DATABASE"]);
        assert!(lookup(&origins, &ConfigPath::new().key("database").key("url")).is_none());
    }

    #[test]
    fn env_origin_map_nested_replaces_flat_winner() {
        let (table, _, winners) = crate::env::env_to_table_with_sources(
            "APP",
            [
                ("APP__DATABASE".into(), "oops".into()),
                ("APP__DATABASE__URL".into(), "x".into()),
            ],
        );
        let origins = origin_map_from_env(&table, &winners);
        assert_eq!(
            env_origin_vars(&origins, "database"),
            ["APP__DATABASE__URL"]
        );
        assert_eq!(
            env_origin_vars(&origins, "database.url"),
            ["APP__DATABASE__URL"]
        );
    }

    #[test]
    fn env_origin_map_case_collision_is_last_writer() {
        let (table, _, winners) = crate::env::env_to_table_with_sources(
            "APP",
            [
                ("APP__host".into(), "first".into()),
                ("APP__HOST".into(), "second".into()),
            ],
        );
        let origins = origin_map_from_env(&table, &winners);
        assert_eq!(env_origin_vars(&origins, "host"), ["APP__HOST"]);
    }
}
