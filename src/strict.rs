//! Cascading strictness — the three knobs that decide whether an unknown
//! key is an error.
//!
//! Phase 3 (#37). Defaults preserve today's behavior; everything is additive.
//!
//! 1. **`strict(bool)`** — whole-resolution default. Existing API, unchanged.
//! 2. **Per-node strictness** — runtime [`Schema::strict`](crate::runtime::Schema::strict)
//!    and static [`ClapfigBuilder::strict_at`](crate::ClapfigBuilder::strict_at) /
//!    [`RuntimeBuilder::strict_at`](crate::RuntimeBuilder::strict_at) set an
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
//! [Knob 1]: crate::ClapfigBuilder::strict

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use toml::Value;

use crate::spec::{FieldKindRef, SchemaRef};

/// Context handed to an [`on_unknown_key`](crate::ClapfigBuilder::on_unknown_key)
/// callback. Carries every signal the callback needs to make a per-key
/// decision: where the key lives in the merged tree, what it was, what
/// file produced it, and which line.
#[derive(Debug)]
pub struct UnknownKeyContext<'a> {
    /// Full dotted path with every segment unquoted, e.g.
    /// `diagnostics.rules.acme.task-due-date-missing`.
    pub path: &'a str,

    /// The single TOML key at the leaf position, as it reached the merge
    /// step — i.e. the final element of the path as TOML parsed it, not
    /// the trailing piece of `path` split on `.`. A bare key like
    /// `missing_footote` gives `leaf = "missing_footote"`; a quoted key
    /// like `"acme.task-due-date-missing"` gives
    /// `leaf = "acme.task-due-date-missing"` (the dots are part of the
    /// key, not segment separators).
    ///
    /// With [`normalize_keys(true)`](crate::ClapfigBuilder::normalize_keys)
    /// the key has been rewritten (kebab → snake) before reaching the
    /// callback, matching the form every other downstream consumer sees.
    /// Callbacks that pattern-match on raw user-supplied spellings
    /// should run on the un-normalized config builder, or normalize the
    /// match arms themselves.
    pub leaf: &'a str,

    /// The value clapfig parsed at this key, before merge into the typed
    /// output.
    pub value: &'a Value,

    /// The file the key came from. `None` when the key came from a non-file
    /// source (env, CLI override, URL query) — strict-mode unknown-key
    /// checking only fires on files today, so this is effectively always
    /// `Some` in Phase 3.
    pub file: Option<&'a Path>,

    /// 1-indexed line number in `file` where the key appears. `None` when
    /// the `find_key_line` heuristic could not locate it (rare; quoted
    /// keys, inline tables).
    pub line: Option<usize>,
}

/// Decision returned by an [`on_unknown_key`](crate::ClapfigBuilder::on_unknown_key)
/// callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownKeyDecision {
    /// Treat the key as a strict-mode violation (default if no callback is
    /// registered). Produces a `ClapfigError::UnknownKeys` entry.
    Reject,
    /// Drop the key silently (same outcome as a lenient subtree).
    Accept,
}

/// Internal type-alias for the boxed callback. `Send + Sync` is required so
/// the hook threads through `Resolver` / `RuntimeResolver`, both of which
/// may be shared across threads.
pub(crate) type UnknownKeyHook =
    Arc<dyn Fn(&UnknownKeyContext<'_>) -> UnknownKeyDecision + Send + Sync>;

/// Flat, path-keyed strictness overrides — the data backing the cascade.
///
/// Built once at `build_resolver` time from:
///
/// - `ClapfigBuilder::strict_at(path, bool)` / `RuntimeBuilder::strict_at`
///   calls (static and runtime paths).
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
}

impl StrictnessOverrides {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, path: impl Into<String>, strict: bool) {
        self.entries.insert(path.into(), strict);
    }

    #[allow(dead_code)] // public via crate-private API; useful for future short-circuits
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `true` when at least one override could promote some key to strict.
    /// Used by the resolve pipeline to decide whether the validate step is
    /// worth running at all — a uniformly-lenient cascade (no `true`
    /// overrides anywhere) lets every unknown key drop silently anyway,
    /// so the per-file walk + `serde_ignored::deserialize` is wasted work
    /// and (worse) changes behavior on the static path by surfacing type
    /// errors that pre-Phase-3 `strict(false)` would have masked.
    pub fn has_any_strict(&self) -> bool {
        self.entries.values().any(|v| *v)
    }

    /// Walk a schema and copy every node's explicit `strict` into the map.
    /// Used to seed overrides from a runtime [`Schema`](crate::runtime::Schema)
    /// at `build_resolver` time.
    pub fn from_schema(schema: SchemaRef<'_>) -> Self {
        let mut out = Self::new();
        walk_schema_strict(schema, "", &mut out);
        out
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
        let initial = section_path_of(path, leaf);
        let mut cursor: String = initial.to_string();
        loop {
            if let Some(v) = self.entries.get(&cursor) {
                return *v;
            }
            // Also probe the bracket-stripped form so an override set on a
            // runtime ArrayOf schema (e.g. `plugins.audit`) is consulted
            // when the unknown key sits inside an array entry
            // (`plugins[0].audit.rogue`).
            let schema_form = strip_brackets(&cursor);
            if schema_form != cursor
                && let Some(v) = self.entries.get(&schema_form)
            {
                return *v;
            }
            if cursor.is_empty() {
                return default;
            }
            cursor = parent_path(&cursor).to_string();
        }
    }
}

/// Recursively visit `schema` and populate `out` with every node whose
/// `strict` is explicitly set.
fn walk_schema_strict(schema: SchemaRef<'_>, prefix: &str, out: &mut StrictnessOverrides) {
    if let Some(value) = schema.strict() {
        out.insert(prefix.to_string(), value);
    }
    for field in schema.fields() {
        let dotted = if prefix.is_empty() {
            field.name.to_string()
        } else {
            format!("{prefix}.{}", field.name)
        };
        match field.kind {
            FieldKindRef::Leaf(_) => {
                // Leaves don't carry a `strict` override.
            }
            FieldKindRef::Nested { schema: nested } | FieldKindRef::ArrayOf { schema: nested } => {
                walk_schema_strict(nested, &dotted, out);
            }
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
/// Uses `strip_suffix` to remove the leaf then the `.` separator. Robust
/// to quoted-key leaves with dots, array-index segments, and any other
/// path shape — the cascade walks the same way regardless of what the
/// leaf looks like.
///
/// `pub(crate)` so `validate::lookup_value` and `validate::find_key_line`
/// can reuse the same single source of truth.
pub(crate) fn section_path_of<'a>(path: &'a str, leaf: &str) -> &'a str {
    if path == leaf {
        return "";
    }
    let parent = path.strip_suffix(leaf).unwrap_or(path);
    parent.strip_suffix('.').unwrap_or(parent)
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
pub(crate) fn resolve_path_kind(schema: SchemaRef<'_>, dotted: &str) -> PathKind {
    if dotted.is_empty() {
        return PathKind::Section;
    }
    let mut current = schema;
    let mut segments = dotted.split('.').peekable();
    while let Some(seg) = segments.next() {
        let mut found = None;
        for field in current.fields() {
            if field.name == seg {
                found = Some(field);
                break;
            }
        }
        let Some(field) = found else {
            return PathKind::Unknown;
        };
        match field.kind {
            FieldKindRef::Leaf(_) => {
                return if segments.peek().is_some() {
                    PathKind::Unknown
                } else {
                    PathKind::Leaf
                };
            }
            FieldKindRef::Nested { schema: nested } | FieldKindRef::ArrayOf { schema: nested } => {
                if segments.peek().is_none() {
                    return PathKind::Section;
                }
                current = nested;
            }
        }
    }
    PathKind::Section
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
    fn has_any_strict_reflects_override_values() {
        let mut overrides = StrictnessOverrides::new();
        assert!(!overrides.has_any_strict());
        overrides.insert("a", false);
        assert!(!overrides.has_any_strict());
        overrides.insert("b", true);
        assert!(overrides.has_any_strict());
    }
}
