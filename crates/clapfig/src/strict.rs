//! Cascading strictness — the three knobs that decide whether an unknown
//! key is an error.
//!
//! Phase 3 (#37). Defaults preserve today's behavior; everything is additive.
//!
//! 1. **`strict(bool)`** — whole-resolution default. Existing API, unchanged.
//! 2. **Per-node strictness** — per-node [`Schema::strict`](crate::runtime::Schema::strict)
//!    and [`Builder::strict_at`](crate::Builder::strict_at) set an
//!    explicit `strict` value on a schema node (or on a dotted path that
//!    resolves to one). The cascade picks the nearest explicit ancestor.
//! 3. **`on_unknown_key(callback)`** — last word for keys the cascade
//!    rejects. The callback sees a [`UnknownKeyContext`] and returns
//!    [`UnknownKeyDecision::Reject`] (default, errors as today) or
//!    [`UnknownKeyDecision::Accept`] (drops silently).
//!
//! # Cascade rule
//!
//! For any unknown key at dotted path `a.b.c`, the effective strictness is
//! the `strict` value of the nearest ancestor schema node (including the
//! key's parent) whose `strict` is explicitly set. If no ancestor sets
//! `strict`, the builder-level default ([Knob 1]) applies.
//!
//! That single rule produces both expected behaviors:
//!
//! - A parent's `strict` value cascades to every descendant that does not
//!   itself set `strict`.
//! - The first descendant that sets its own `strict` becomes the new root
//!   for its subtree, overriding the inherited value below it.
//!
//! [Knob 1]: crate::Builder::strict

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::ClapfigError;
use crate::format::Span;
use crate::runtime::{DocumentRoot, Schema, Shape, TaggedShape};
use crate::types::InputType;
use crate::value::{Map, Value};

/// Context handed to an [`on_unknown_key`](crate::Builder::on_unknown_key)
/// callback. Carries every signal the callback needs to make a per-key
/// decision: where the key lives in the merged tree, what it was, what
/// file produced it, and which line (from the file's span index).
#[derive(Debug)]
pub struct UnknownKeyContext<'a> {
    /// Full dotted path with every segment unquoted, e.g.
    /// `diagnostics.rules.acme.task-due-date-missing`.
    pub path: &'a str,

    /// The single raw key at the leaf position, as it reached the merge
    /// step — i.e. the final element of the path as the source format
    /// parsed it, not the trailing piece of `path` split on `.`. A bare
    /// key like
    /// `missing_footote` gives `leaf = "missing_footote"`; a quoted key
    /// like `"acme.task-due-date-missing"` gives
    /// `leaf = "acme.task-due-date-missing"` (the dots are part of the
    /// key, not segment separators).
    ///
    /// With [`normalize_keys(true)`](crate::Builder::normalize_keys)
    /// the key has been rewritten (kebab → snake) before reaching the
    /// callback, matching the form every other downstream consumer sees.
    /// Callbacks that pattern-match on raw user-supplied spellings
    /// should run on the un-normalized config builder, or normalize the
    /// match arms themselves.
    pub leaf: &'a str,

    /// The value clapfig parsed at this key, before merge into the typed
    /// output.
    ///
    /// `None` only when the lookup genuinely cannot resolve the value —
    /// e.g. an out-of-bounds array index, or a path that crosses a
    /// non-table intermediate. In practice the callback nearly always
    /// sees `Some(_)`; an `Option` here is more honest than a stand-in
    /// `Value::Boolean(false)` that pattern-matching on `.as_bool()`
    /// would silently consume.
    pub value: Option<&'a Value>,

    /// The file the key came from. `None` when the key came from a
    /// non-file source — for an env-derived key the callback sees
    /// `file: None` and a `path` whose segments mirror the variable's
    /// `__`-separated pieces.
    pub file: Option<&'a Path>,

    /// 1-indexed line number in `file` where the key appears. `None` when
    /// the file's span index has no entry for the key, or the origin is
    /// not a file. Derived from [`span`](Self::span).
    pub line: Option<usize>,

    /// Byte span of the **key** token (ADR-0006). Set from the file's
    /// span index when that path has a key token; `None` when the index
    /// has no entry or the origin is not a file.
    pub span: Option<Span>,

    /// Environment variable that supplied this key, when it came from
    /// the env layer.
    pub env_var: Option<&'a str>,

    /// URL query-parameter key that supplied this key, when it came from
    /// the URL layer.
    pub url_key: Option<&'a str>,

    /// Override key that supplied this key, when it came from a
    /// programmatic override (`cli_override` / `cli_overrides_from`).
    pub override_key: Option<&'a str>,

    /// Which input type produced the key. `None` when unset.
    pub input_type: Option<InputType>,
}

/// Decision returned by an [`on_unknown_key`](crate::Builder::on_unknown_key)
/// callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownKeyDecision {
    /// Treat the key as a strict-mode violation (default if no callback is
    /// registered). Produces a `ClapfigError::UnknownKeys` entry.
    Reject,
    /// Drop the key silently (same outcome as a lenient subtree).
    Accept,
    /// Route the key into the collected-unknowns list returned by
    /// [`load_with_unknowns`](crate::Builder::load_with_unknowns).
    /// The key is NOT a strict-mode violation — load succeeds — but the
    /// caller can inspect the list to surface diagnostic-style "we saw
    /// this key, it wasn't in the schema, here's what to do" feedback
    /// without rebuilding the loader's parse/merge pipeline themselves.
    Collect,
}

/// One collected unknown-key entry returned alongside the loaded config
/// from [`load_with_unknowns`](crate::Builder::load_with_unknowns).
///
/// Produced when an [`on_unknown_key`](crate::Builder::on_unknown_key)
/// callback returns [`UnknownKeyDecision::Collect`] for a key the
/// strictness cascade flagged. Owned variant of [`UnknownKeyContext`] —
/// the values are cloned out of the parsed table so the caller can use
/// the list after the resolver / merged table have been dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectedUnknown {
    /// Full dotted path, e.g. `diagnostics.rules.acme.task-due-date`.
    pub path: String,
    /// Raw key at the leaf position. See
    /// [`UnknownKeyContext::leaf`] for the quoted-key semantics.
    pub leaf: String,
    /// Parsed value cloned out of the source table. `None` when the
    /// lookup couldn't resolve (out-of-bounds array index, path through
    /// a non-table) — matches the `None` semantics of
    /// [`UnknownKeyContext::value`].
    pub value: Option<Value>,
    /// File the key came from, when sourced from a config file.
    pub file: Option<std::path::PathBuf>,
    /// 1-indexed line number in `file`, if the span index located the
    /// key. See [`UnknownKeyContext::line`].
    pub line: Option<usize>,
    /// Byte span of the **key** token (ADR-0006). Set from the file's
    /// span index when that path has a key token; `None` when the index
    /// has no entry or the origin is not a file.
    pub span: Option<Span>,
    /// Environment variable that supplied this key, when it came from
    /// the env layer.
    pub env_var: Option<String>,
    /// URL query-parameter key that supplied this key, when it came from
    /// the URL layer.
    pub url_key: Option<String>,
    /// Override key that supplied this key, when it came from a
    /// programmatic override.
    pub override_key: Option<String>,
    /// Which input type produced the key. `None` when unset.
    pub input_type: Option<InputType>,
}

/// Internal type-alias for the boxed callback. `Send + Sync` is required so
/// the hook threads through [`Resolver`](crate::Resolver), which may be
/// shared across threads.
pub(crate) type UnknownKeyHook =
    Arc<dyn Fn(&UnknownKeyContext<'_>) -> UnknownKeyDecision + Send + Sync>;

/// Build an `on_unknown_key` callback implementing the "accept dotted,
/// reject bare" pattern: under the configured `path` subtree, an unknown
/// key whose raw TOML leaf contains a `.` is treated as a schema-
/// extension key and resolved with `decision` (typically
/// [`UnknownKeyDecision::Accept`] for CLI hosts or
/// [`UnknownKeyDecision::Collect`] for LSP hosts that want to surface
/// them to users); any other unknown key falls through to
/// [`UnknownKeyDecision::Reject`].
///
/// `path` bounds where the rule applies. `""` means "anywhere in the
/// schema"; `"diagnostics.rules"` means "only under the `diagnostics.rules`
/// subtree." A path-segment boundary is enforced: `"diag"` won't match a
/// key under `"diagnostics.rules"`.
///
/// The cascade still fires first — keys flagged lenient by `strict_at`
/// drop silently without the callback ever running. This helper only
/// decides what to do with keys the cascade decided to reject.
pub(crate) fn dotted_extension_callback(
    path: String,
    decision: UnknownKeyDecision,
) -> UnknownKeyHook {
    Arc::new(move |ctx: &UnknownKeyContext<'_>| {
        let in_subtree = path.is_empty()
            || ctx.path == path
            || ctx
                .path
                .strip_prefix(&path)
                .is_some_and(|rest| rest.starts_with('.'));
        if in_subtree && ctx.leaf.contains('.') {
            decision
        } else {
            UnknownKeyDecision::Reject
        }
    })
}

/// Flat, path-keyed strictness overrides — the data backing the cascade.
///
/// Built once at `build_resolver` time from:
///
/// - `Builder::strict_at(path, bool)` calls (and, through the
///   forwarding `TypedBuilder`, the typed path's `strict_at`).
/// - Walking a runtime [`Schema`](crate::runtime::Schema) and copying every
///   node where `strict.is_some()` into the same map.
///
/// Insertion order matters when both sources provide a value for the same
/// path: the builder overlay (`strict_at`) wins because it is the most
/// local explicit statement (per the proposal). Callers handle that by
/// inserting schema-derived entries first, then builder-derived entries.
#[derive(Debug, Default, Clone)]
pub(crate) struct StrictnessOverrides {
    entries: HashMap<String, bool>,
    /// When the document root is a homogeneous Map, item-schema `strict`
    /// annotations are stored root-relative (`db`), while runtime paths
    /// include the dynamic entry key (`core.db.rogue`). Cascade lookup
    /// probes only the path with that first segment stripped.
    skip_root_entry: bool,
    /// Builder `strict_at` pairs, recorded so phase 2 can rebuild
    /// schema-derived entries from the selected variant and replay these
    /// overlays on top (builder wins at the same path).
    builder_overlay: Vec<(String, bool)>,
}

impl StrictnessOverrides {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            skip_root_entry: false,
            builder_overlay: Vec::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert(&mut self, path: impl Into<String>, strict: bool) {
        self.entries.insert(path.into(), strict);
    }

    /// Schema-derived insert: conflicting values at the same path from
    /// sibling variants union to `true` (any-strict-wins, declaration
    /// order independent). Never used for builder `strict_at`.
    fn insert_schema(&mut self, path: impl Into<String>, strict: bool) {
        let path = path.into();
        match self.entries.get(&path) {
            None => {
                self.entries.insert(path, strict);
            }
            Some(&existing) if existing == strict => {}
            Some(_) => {
                self.entries.insert(path, true);
            }
        }
    }

    /// Builder `strict_at` overlay: overwrites schema-derived entries at
    /// the same path and is replayed after phase-2 selected-variant rebuild.
    fn overlay(&mut self, path: impl Into<String>, strict: bool) {
        let path = path.into();
        self.entries.insert(path.clone(), strict);
        self.builder_overlay.push((path, strict));
    }

    /// Phase-2 map: schema-derived strictness from the selected tagged
    /// branch (and nested selected tagged objects), then builder overlays.
    pub(crate) fn for_selected_branch(&self, root: DocumentRoot<'_>, merged: &Map) -> Self {
        let mut out = Self::new();
        out.skip_root_entry = self.skip_root_entry;
        match root {
            DocumentRoot::Object(schema) => {
                walk_selected_object(schema, Some(merged), "", &mut out);
            }
            DocumentRoot::Map(map) => {
                out.skip_root_entry = true;
                if let Some(value) = map.strict {
                    out.insert_schema(String::new(), value);
                }
                for (key, value) in merged {
                    walk_selected_shape(
                        &map.item,
                        Some(value),
                        &format!("{key}"),
                        &mut out,
                    );
                }
            }
            DocumentRoot::Tagged(tagged) => {
                walk_selected_tagged(tagged, Some(merged), "", &mut out);
            }
        }
        for (path, strict) in &self.builder_overlay {
            out.entries.insert(path.clone(), *strict);
        }
        out
    }

    /// `true` when at least one override could promote some key to strict.
    /// Used by the resolve pipeline to decide whether the validate step is
    /// worth running at all — a uniformly-lenient cascade (no `true`
    /// overrides anywhere) lets every unknown key drop silently anyway,
    /// so the per-file and env-layer walks would be pure wasted work.
    pub fn has_any_strict(&self) -> bool {
        self.entries.values().any(|v| *v)
    }

    /// Walk a schema and copy every node's explicit `strict` into the map.
    /// Used to seed overrides from a [`Schema`](crate::runtime::Schema)
    /// at `build_resolver` time.
    pub fn from_schema(schema: &Schema) -> Self {
        let mut out = Self::new();
        walk_schema_strict(schema, "", &mut out);
        out
    }

    pub(crate) fn from_root(root: DocumentRoot<'_>) -> Self {
        match root {
            DocumentRoot::Object(schema) => Self::from_schema(schema),
            DocumentRoot::Map(map) => {
                let mut out = Self::new();
                out.skip_root_entry = true;
                if let Some(value) = map.strict {
                    out.insert(String::new(), value);
                }
                walk_shape_strict(&map.item, "", &mut out);
                out
            }
            DocumentRoot::Tagged(tagged) => {
                let mut out = Self::new();
                walk_tagged_strict(tagged, "", &mut out);
                out
            }
        }
    }

    /// Resolve the effective strictness for an unknown key at `(path, leaf)`.
    ///
    /// `path` is the dotted form (full key, including the leaf); `leaf` is
    /// the raw TOML key the parser saw at the leaf position. Passing the
    /// leaf separately is necessary for two cases:
    ///
    /// - **Quoted leaves with dots** (`diagnostics.rules."acme.task"`):
    ///   the section path is `diagnostics.rules`, not
    ///   `diagnostics.rules.acme`. Dot-splitting the path would treat the
    ///   leaf's internal dots as ancestor separators and apply overrides
    ///   meant for unrelated sections.
    /// - **Array-indexed paths** (`plugins[0].rogue`): the cascade
    ///   probes both the physical form (`plugins[0]`) and the
    ///   bracket-stripped schema form (`plugins`) at each step, so an
    ///   override set on the item schema applies to any entry.
    ///
    /// The cascade walks from the leaf's section path upward, returning
    /// the first explicit override found. With no override on any
    /// ancestor, `default` is returned.
    pub fn effective_strict(&self, path: &str, leaf: &str, default: bool) -> bool {
        // Walk subslices of the original `path` to avoid an allocation per
        // step — `HashMap<String, _>::get` accepts `&str` via the `Borrow`
        // impl. The only allocation in the loop body is `strip_brackets`,
        // which we now skip when the cursor has no brackets to strip.
        let mut cursor: &str = section_path_of(path, leaf);
        loop {
            if let Some(v) = self.probe(cursor) {
                return v;
            }
            if cursor.is_empty() {
                return default;
            }
            cursor = parent_path(cursor);
        }
    }

    fn probe(&self, cursor: &str) -> Option<bool> {
        if self.skip_root_entry {
            if cursor.is_empty() {
                return self.entries.get("").copied();
            }
            if cursor.contains('[') {
                let schema_form = strip_brackets(cursor);
                if let Some(v) = self.entries.get(strip_first_segment(&schema_form)) {
                    return Some(*v);
                }
            }
            return self.entries.get(strip_first_segment(cursor)).copied();
        }
        if let Some(v) = self.entries.get(cursor) {
            return Some(*v);
        }
        if cursor.contains('[') {
            let schema_form = strip_brackets(cursor);
            if let Some(v) = self.entries.get(&schema_form) {
                return Some(*v);
            }
        }
        None
    }
}

/// Recursively visit `schema` and populate `out` with every node whose
/// `strict` is explicitly set.
fn walk_schema_strict(schema: &Schema, prefix: &str, out: &mut StrictnessOverrides) {
    if let Some(value) = schema.strict {
        out.insert_schema(prefix.to_string(), value);
    }
    for field in &schema.fields {
        let dotted = if prefix.is_empty() {
            field.name.to_string()
        } else {
            format!("{prefix}.{}", field.name)
        };
        walk_shape_strict(&field.field, &dotted, out);
    }
}

fn walk_shape_strict(shape: &Shape, dotted: &str, out: &mut StrictnessOverrides) {
    match shape {
        Shape::Leaf(_) => {}
        Shape::Object(nested) => walk_schema_strict(nested, dotted, out),
        Shape::Array(array) => {
            if let Some(value) = array.strict {
                out.insert_schema(dotted.to_string(), value);
            }
            walk_shape_strict(&array.item, dotted, out);
        }
        Shape::Map(map) => {
            if let Some(value) = map.strict {
                out.insert_schema(dotted.to_string(), value);
            }
            walk_shape_strict(&map.item, dotted, out);
        }
        Shape::Tagged(tagged) => walk_tagged_strict(tagged, dotted, out),
    }
}

/// Phase-1 tagged walk: the tagged node owns `dotted` (spec: the tagged
/// object is the cascade parent). Variant root `schema.strict` does not
/// write that path. Nested fields of every variant union with
/// any-strict-wins so declaration order does not change behavior.
fn walk_tagged_strict(tagged: &TaggedShape, dotted: &str, out: &mut StrictnessOverrides) {
    if let Some(value) = tagged.strict {
        out.insert_schema(dotted.to_string(), value);
    }
    for variant in &tagged.variants {
        for field in &variant.schema.fields {
            let child = if dotted.is_empty() {
                field.name.clone()
            } else {
                format!("{dotted}.{}", field.name)
            };
            walk_shape_strict(&field.field, &child, out);
        }
    }
}

fn walk_selected_object(
    schema: &Schema,
    table: Option<&Map>,
    prefix: &str,
    out: &mut StrictnessOverrides,
) {
    if let Some(value) = schema.strict {
        out.insert_schema(prefix.to_string(), value);
    }
    for field in &schema.fields {
        let dotted = if prefix.is_empty() {
            field.name.to_string()
        } else {
            format!("{prefix}.{}", field.name)
        };
        walk_selected_shape(
            &field.field,
            table.and_then(|t| t.get(&field.name)),
            &dotted,
            out,
        );
    }
}

fn walk_selected_tagged(
    tagged: &TaggedShape,
    table: Option<&Map>,
    dotted: &str,
    out: &mut StrictnessOverrides,
) {
    if let Some(value) = tagged.strict {
        out.insert_schema(dotted.to_string(), value);
    }
    let Some(table) = table else {
        return;
    };
    let Some(selected) = tagged.selected(table) else {
        return;
    };
    for field in &selected.schema.fields {
        let child = if dotted.is_empty() {
            field.name.clone()
        } else {
            format!("{dotted}.{}", field.name)
        };
        walk_selected_shape(&field.field, table.get(&field.name), &child, out);
    }
}

fn walk_selected_shape(
    shape: &Shape,
    value: Option<&Value>,
    dotted: &str,
    out: &mut StrictnessOverrides,
) {
    match shape {
        Shape::Leaf(_) => {}
        Shape::Object(nested) => {
            walk_selected_object(nested, value.and_then(Value::as_map), dotted, out);
        }
        Shape::Array(array) => {
            if let Some(v) = array.strict {
                out.insert_schema(dotted.to_string(), v);
            }
            walk_shape_strict(&array.item, dotted, out);
        }
        Shape::Map(map) => {
            if let Some(v) = map.strict {
                out.insert_schema(dotted.to_string(), v);
            }
            walk_shape_strict(&map.item, dotted, out);
        }
        Shape::Tagged(tagged) => {
            walk_selected_tagged(tagged, value.and_then(Value::as_map), dotted, out);
        }
    }
}

/// Trim the last path segment (whether a `.field` or an `[index]`) from a
/// dotted path, yielding the parent. Returns `""` for a single-segment
/// path or an already-empty path.
///
/// Handling both `.` and `[` lets the cascade walk through array-indexed
/// paths like `plugins[0].name` → `plugins[0]` → `plugins` so a
/// `strict_at("plugins", false)` override applies to keys nested inside
/// array entries.
fn parent_path(path: &str) -> &str {
    let dot = path.rfind('.');
    let bracket = path.rfind('[');
    match (dot, bracket) {
        (Some(d), Some(b)) => &path[..d.max(b)],
        (Some(d), None) => &path[..d],
        (None, Some(b)) => &path[..b],
        (None, None) => "",
    }
}

/// Section path of `(path, leaf)`: `path` with the trailing leaf stripped
/// (plus the `.` separator if any). Returns `""` for a top-level key.
///
/// Shared with the strictness cascade —
/// dot-splitting `path` would miscount segments when the leaf is a
/// quoted TOML key containing literal dots (e.g.
/// `"acme.task-due-date-missing"`). Stripping the known leaf off the
/// end is the only way to recover the correct section path. Value and
/// span lookup walk the structured [`crate::format::ConfigPath`] instead.
pub(crate) fn section_path_of<'a>(path: &'a str, leaf: &str) -> &'a str {
    path.strip_suffix(leaf)
        .map(|p| p.strip_suffix('.').unwrap_or(p))
        .unwrap_or("")
}

/// Strip every `[N]` array-index segment from a dotted path, yielding the
/// schema-style form. `plugins[0].audit` → `plugins.audit`; `a.b.c` is
/// unchanged.
fn strip_brackets(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut in_brackets = false;
    for ch in path.chars() {
        match ch {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            _ if in_brackets => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Resolve a dotted path against a schema and return the kind of the node
/// it lands on (`Nested`, `ArrayOf`, or `Leaf`). Used to validate
/// `strict_at` paths at `build_resolver` time.
fn strip_first_segment(path: &str) -> &str {
    match path.find('.') {
        Some(i) => &path[i + 1..],
        None => "",
    }
}

fn resolve_path_kind_root(root: DocumentRoot<'_>, dotted: &str) -> PathKind {
    match root {
        DocumentRoot::Object(schema) => resolve_path_kind(schema, dotted),
        DocumentRoot::Map(_) => {
            if dotted.is_empty() {
                PathKind::Section
            } else {
                PathKind::Unknown
            }
        }
        DocumentRoot::Tagged(tagged) => resolve_tagged_kind(tagged, dotted),
    }
}

fn resolve_tagged_kind(tagged: &TaggedShape, rest: &str) -> PathKind {
    if rest.is_empty() {
        return PathKind::Section;
    }
    if rest == tagged.tag {
        return PathKind::Leaf;
    }
    if rest.starts_with(&format!("{}.", tagged.tag)) {
        return PathKind::Unknown;
    }
    union_path_kind(
        tagged
            .variants
            .iter()
            .map(|v| resolve_path_kind(&v.schema, rest)),
    )
}

/// Section in any variant wins (valid `strict_at` target). Leaf only
/// when every variant that knows the path treats it as a leaf.
fn union_path_kind(kinds: impl IntoIterator<Item = PathKind>) -> PathKind {
    let mut saw_section = false;
    let mut saw_leaf = false;
    for kind in kinds {
        match kind {
            PathKind::Section => saw_section = true,
            PathKind::Leaf => saw_leaf = true,
            PathKind::Unknown => {}
        }
    }
    if saw_section {
        PathKind::Section
    } else if saw_leaf {
        PathKind::Leaf
    } else {
        PathKind::Unknown
    }
}

pub(crate) fn resolve_path_kind(schema: &Schema, dotted: &str) -> PathKind {
    if dotted.is_empty() {
        return PathKind::Section;
    }
    let mut current = schema;
    let mut segments = dotted.split('.').peekable();
    while let Some(seg) = segments.next() {
        let Some(field) = current.fields.iter().find(|f| f.name == seg) else {
            return PathKind::Unknown;
        };
        match &field.field {
            crate::runtime::Shape::Leaf(_) => {
                return if segments.peek().is_some() {
                    PathKind::Unknown
                } else {
                    PathKind::Leaf
                };
            }
            shape if shape.is_value_field() => {
                return if segments.peek().is_some() {
                    PathKind::Unknown
                } else {
                    PathKind::Leaf
                };
            }
            crate::runtime::Shape::Object(nested) => {
                if segments.peek().is_none() {
                    return PathKind::Section;
                }
                current = nested;
            }
            crate::runtime::Shape::Array(_) | crate::runtime::Shape::Map(_) => {
                if segments.peek().is_none() {
                    return PathKind::Section;
                }
                let rest = remaining_dotted(segments);
                return match field.field.peel_containers() {
                    crate::runtime::Shape::Object(nested) => resolve_path_kind(nested, &rest),
                    crate::runtime::Shape::Tagged(tagged) => resolve_tagged_kind(tagged, &rest),
                    crate::runtime::Shape::Leaf(_) => PathKind::Unknown,
                    crate::runtime::Shape::Array(_) | crate::runtime::Shape::Map(_) => {
                        unreachable!("peel_containers strips Array/Map")
                    }
                };
            }
            crate::runtime::Shape::Tagged(tagged) => {
                if segments.peek().is_none() {
                    return PathKind::Section;
                }
                return resolve_tagged_kind(tagged, &remaining_dotted(segments));
            }
        }
    }
    PathKind::Section
}

fn remaining_dotted<'a>(segments: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = segments.collect();
    parts.join(".")
}

/// Validate a list of `(path, strict)` overrides against a schema and
/// collect them into a [`StrictnessOverrides`], seeded from the schema's
/// own per-node `strict` settings (builder-supplied `strict_at` entries
/// override schema-derived ones for the same path).
///
/// Errors as [`ClapfigError::InvalidStrictPath`] when:
///
/// - the path does not resolve to any field in the schema, or
/// - the path resolves to a leaf field (strict is a container property).
///
/// When `normalize_keys` is `true`, each path is rewritten through
/// `normalize::normalize_key` before lookup so the override accepts the
/// same kebab/snake spellings the rest of the pipeline does.
pub(crate) fn build_strict_overrides_root(
    entries: &[(String, bool)],
    normalize_keys: bool,
    root: DocumentRoot<'_>,
) -> Result<StrictnessOverrides, ClapfigError> {
    let mut out = StrictnessOverrides::from_root(root);
    for (raw_path, strict) in entries {
        let path = if normalize_keys {
            crate::normalize::normalize_key(raw_path)
        } else {
            raw_path.clone()
        };
        match resolve_path_kind_root(root, &path) {
            PathKind::Section => out.overlay(path, *strict),
            PathKind::Leaf => {
                return Err(ClapfigError::InvalidStrictPath {
                    path: raw_path.clone(),
                    reason: "path resolves to a leaf field, but strict is a section property"
                        .into(),
                });
            }
            PathKind::Unknown => {
                return Err(ClapfigError::InvalidStrictPath {
                    path: raw_path.clone(),
                    reason: "path does not resolve to any field in the config schema".into(),
                });
            }
        }
    }
    Ok(out)
}

/// Result of [`resolve_path_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    /// Path resolves to a nested-object node (the only valid `strict_at`
    /// target).
    Section,
    /// Path resolves to a leaf field — invalid as a `strict_at` target.
    Leaf,
    /// Path does not resolve to any field in the schema.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Field;

    #[test]
    fn parent_path_works() {
        assert_eq!(parent_path("a.b.c"), "a.b");
        assert_eq!(parent_path("a.b"), "a");
        assert_eq!(parent_path("a"), "");
        assert_eq!(parent_path(""), "");
    }

    #[test]
    fn parent_path_handles_array_indices() {
        // Without bracket-awareness, `plugins[0].name` would walk to
        // `plugins[0]` then to `""` (skipping `plugins`), so an
        // `array_of("plugins", ...).strict(false)` override would never
        // apply to keys nested inside array entries.
        assert_eq!(parent_path("plugins[0].name"), "plugins[0]");
        assert_eq!(parent_path("plugins[0]"), "plugins");
        assert_eq!(parent_path("plugins[10].a.b"), "plugins[10].a");
    }

    #[test]
    fn cascade_walks_through_array_indices() {
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("plugins", false);
        // Unknown key inside `plugins[0]` should pick up the `plugins`
        // override via the indexed-path cascade.
        assert!(!overrides.effective_strict("plugins[0].rogue", "rogue", true));
    }

    #[test]
    fn cascade_returns_default_with_no_overrides() {
        let overrides = StrictnessOverrides::new();
        assert!(overrides.effective_strict("any.path.here", "here", true));
        assert!(!overrides.effective_strict("any.path.here", "here", false));
    }

    #[test]
    fn cascade_uses_nearest_ancestor() {
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("a", true);
        overrides.insert("a.b", false);
        // Unknown key at "a.b.c": parent is "a.b" — explicit false wins.
        assert!(!overrides.effective_strict("a.b.c", "c", true));
        // Unknown key at "a.x": parent is "a" — explicit true wins.
        assert!(overrides.effective_strict("a.x", "x", false));
    }

    #[test]
    fn descendant_can_re_tighten() {
        // The "the first descendant that sets its own strict becomes the
        // new root" test from the proposal.
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("plugins", false);
        overrides.insert("plugins.audit", true);
        // Lenient subtree under `plugins`:
        assert!(!overrides.effective_strict("plugins.foo.bar", "bar", true));
        // Re-tightened under `plugins.audit`:
        assert!(overrides.effective_strict("plugins.audit.x", "x", false));
    }

    #[test]
    fn root_override_applies_when_no_more_specific() {
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("", false);
        assert!(!overrides.effective_strict("anything", "anything", true));
    }

    #[test]
    fn cascade_uses_section_path_not_dot_split_for_quoted_leaves() {
        // For `diagnostics.rules."acme.task"`, the path string is
        // `diagnostics.rules.acme.task` but leaf is `acme.task`. The
        // section path is `diagnostics.rules` — an override on
        // `diagnostics.rules.acme` is for an unrelated sibling and must
        // NOT apply to the quoted-leaf key.
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("diagnostics.rules.acme", true);
        assert!(!overrides.effective_strict("diagnostics.rules.acme.task", "acme.task", false,));
    }

    #[test]
    fn cascade_probes_bracket_stripped_form_at_each_step() {
        // For `plugins[0].audit.rogue`, an override stored at
        // `plugins.audit` (from `strict_at("plugins.audit", false)` or a
        // runtime ArrayOf item-schema's `audit.strict(...)`) should be
        // consulted on the schema-form walk: `plugins[0].audit` →
        // bracket-stripped `plugins.audit` hits.
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("plugins.audit", false);
        assert!(!overrides.effective_strict("plugins[0].audit.rogue", "rogue", true,));
    }

    #[test]
    fn strip_brackets_removes_array_indices() {
        assert_eq!(strip_brackets("plugins[0].audit"), "plugins.audit");
        assert_eq!(strip_brackets("a[10].b[2].c"), "a.b.c");
        assert_eq!(strip_brackets("a.b.c"), "a.b.c");
        assert_eq!(strip_brackets(""), "");
    }

    #[test]
    fn from_schema_records_array_and_map_node_strict() {
        use crate::runtime::Shape;
        let plugin = Schema::object("Plugin").field("name", Field::string().optional());
        let schema = Schema::object("App")
            .field(
                "plugins",
                Shape::array("plugins", plugin.clone()).strict(false),
            )
            .field("servers", Shape::map("servers", plugin).strict(false))
            .build();
        let overrides = StrictnessOverrides::from_schema(&schema);
        assert!(
            !overrides.effective_strict("plugins[0].rogue", "rogue", true),
            "array-node strict(false) must govern unknown keys in nested items"
        );
        assert!(
            !overrides.effective_strict("servers.core.rogue", "rogue", true),
            "map-node strict(false) must govern unknown keys in nested items"
        );
    }

    #[test]
    fn resolve_path_kind_walks_through_nested_containers() {
        let schema = Schema::object("App")
            .field(
                "containers",
                Field::array_of_type(Field::array_of_type(Schema::object("Item").nested(
                    "policy",
                    Schema::object("Policy").field("name", Field::string().optional()),
                ))),
            )
            .field(
                "groups",
                Field::map_of(Field::array_of_type(
                    Schema::object("Item").field("timeout", Field::integer().optional()),
                )),
            )
            .build();
        assert_eq!(resolve_path_kind(&schema, "containers"), PathKind::Section);
        assert_eq!(
            resolve_path_kind(&schema, "containers.policy"),
            PathKind::Section
        );
        assert_eq!(
            resolve_path_kind(&schema, "containers.policy.name"),
            PathKind::Leaf
        );
        assert_eq!(resolve_path_kind(&schema, "groups"), PathKind::Section);
        assert_eq!(resolve_path_kind(&schema, "groups.timeout"), PathKind::Leaf);
    }

    #[test]
    fn has_any_strict_reflects_override_values() {
        let mut overrides = StrictnessOverrides::new();
        assert!(!overrides.has_any_strict());
        overrides.insert("a", false);
        assert!(!overrides.has_any_strict());
        overrides.insert("b", true);
        assert!(overrides.has_any_strict());
    }

    #[test]
    fn unknown_key_context_origin_facts_construct() {
        let value = Value::Integer(1);
        let ctx = UnknownKeyContext {
            path: "plugins[3].host",
            leaf: "host",
            value: Some(&value),
            file: Some(Path::new("/tmp/app.toml")),
            line: Some(4),
            span: Some(Span { start: 10, end: 14 }),
            env_var: None,
            url_key: None,
            override_key: None,
            input_type: Some(InputType::File),
        };
        assert_eq!(ctx.path, "plugins[3].host");
        assert_eq!(ctx.span, Some(Span { start: 10, end: 14 }));
        assert_eq!(ctx.input_type, Some(InputType::File));
        assert!(ctx.env_var.is_none());
        assert!(ctx.url_key.is_none());
    }

    #[test]
    fn collected_unknown_mirrors_context_origin_facts() {
        let collected = CollectedUnknown {
            path: "rogue".into(),
            leaf: "rogue".into(),
            value: Some(Value::Boolean(true)),
            file: None,
            line: None,
            span: None,
            env_var: Some("MYAPP__ROGUE".into()),
            url_key: None,
            override_key: None,
            input_type: Some(InputType::Env),
        };
        assert_eq!(collected.env_var.as_deref(), Some("MYAPP__ROGUE"));
        assert_eq!(collected.input_type, Some(InputType::Env));
        assert!(collected.span.is_none());
        assert!(collected.url_key.is_none());
    }

    #[test]
    fn tagged_root_strict_is_not_overwritten_by_variant_schema_strict() {
        let tagged = Shape::tagged("Block", "kind")
            .strict(false)
            .variant(
                "rust",
                Schema::object("Rust")
                    .strict(true)
                    .field("mount", Field::string())
                    .build(),
            )
            .variant(
                "payload",
                Schema::object("Payload")
                    .strict(true)
                    .field("artifact", Field::string())
                    .build(),
            )
            .build();
        let overrides = StrictnessOverrides::from_root(DocumentRoot::Tagged(&tagged));
        assert!(
            !overrides.effective_strict("crate_path", "crate_path", true),
            "tagged.strict(false) must govern keys at the tagged object"
        );
    }

    #[test]
    fn sibling_variant_nested_strict_union_is_declaration_order_independent() {
        let rust_lenient_params = Schema::object("Rust")
            .nested(
                "params",
                Schema::object("P")
                    .strict(false)
                    .field("shape", Field::string().optional()),
            )
            .build();
        let payload_strict_params = Schema::object("Payload")
            .nested(
                "params",
                Schema::object("Q")
                    .strict(true)
                    .field("artifact", Field::string().optional()),
            )
            .build();
        let a = Shape::tagged("Block", "kind")
            .variant("rust", rust_lenient_params.clone())
            .variant("payload", payload_strict_params.clone())
            .build();
        let b = Shape::tagged("Block", "kind")
            .variant("payload", payload_strict_params)
            .variant("rust", rust_lenient_params)
            .build();
        let first = StrictnessOverrides::from_root(DocumentRoot::Tagged(&a));
        let reversed = StrictnessOverrides::from_root(DocumentRoot::Tagged(&b));
        assert_eq!(
            first.effective_strict("params.rogue", "rogue", false),
            reversed.effective_strict("params.rogue", "rogue", false),
        );
        assert!(
            first.effective_strict("params.rogue", "rogue", false),
            "any-strict-wins: one variant's params.strict(true) makes phase 1 strict"
        );
    }

    #[test]
    fn resolve_path_kind_walks_nested_tagged_as_section() {
        let schema = Schema::object("App")
            .field(
                "block",
                Shape::from(
                    Shape::tagged("Block", "kind")
                        .variant(
                            "rust",
                            Schema::object("Rust")
                                .field("mount", Field::string())
                                .build(),
                        )
                        .variant(
                            "payload",
                            Schema::object("Payload")
                                .field("artifact", Field::string())
                                .build(),
                        )
                        .build(),
                ),
            )
            .build();
        assert_eq!(resolve_path_kind(&schema, "block"), PathKind::Section);
        assert_eq!(resolve_path_kind(&schema, "block.kind"), PathKind::Leaf);
        assert_eq!(resolve_path_kind(&schema, "block.mount"), PathKind::Leaf);
        assert_eq!(resolve_path_kind(&schema, "block.artifact"), PathKind::Leaf);
    }

    #[test]
    fn resolve_path_kind_section_wins_when_any_variant_is_a_section() {
        let tagged = Shape::tagged("Block", "kind")
            .variant(
                "a",
                Schema::object("A").field("params", Field::string()).build(),
            )
            .variant(
                "b",
                Schema::object("B")
                    .nested(
                        "params",
                        Schema::object("P").field("x", Field::string().optional()),
                    )
                    .build(),
            )
            .build();
        assert_eq!(
            resolve_path_kind_root(DocumentRoot::Tagged(&tagged), "params"),
            PathKind::Section
        );
    }
}
