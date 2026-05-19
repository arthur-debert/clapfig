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

    /// The single TOML key clapfig saw at the leaf position — i.e. the
    /// final element of the path as TOML parsed it, not the trailing piece
    /// of `path` split on `.`. A bare key like `missing_footote` gives
    /// `leaf = "missing_footote"`; a quoted key like
    /// `"acme.task-due-date-missing"` gives
    /// `leaf = "acme.task-due-date-missing"` (the dots are part of the
    /// key, not segment separators).
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

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Walk a schema and copy every node's explicit `strict` into the map.
    /// Used to seed overrides from a runtime [`Schema`](crate::runtime::Schema)
    /// at `build_resolver` time.
    pub fn from_schema(schema: SchemaRef<'_>) -> Self {
        let mut out = Self::new();
        walk_schema_strict(schema, "", &mut out);
        out
    }

    /// Resolve the effective strictness for an unknown key.
    ///
    /// The cascade walks `dotted_path` from its leaf parent up to the root,
    /// returning the first explicit override it finds. With no override on
    /// any ancestor, `default` is returned.
    pub fn effective_strict(&self, dotted_path: &str, default: bool) -> bool {
        // Parent of "a.b.c" is "a.b"; parent of "a" is ""; parent of "" is "".
        // Walk until we find a hit, or hit root and return default.
        let mut cursor: &str = parent_path(dotted_path);
        loop {
            if let Some(v) = self.entries.get(cursor) {
                return *v;
            }
            if cursor.is_empty() {
                return default;
            }
            cursor = parent_path(cursor);
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
        assert!(!overrides.effective_strict("plugins[0].rogue", true));
    }

    #[test]
    fn cascade_returns_default_with_no_overrides() {
        let overrides = StrictnessOverrides::new();
        assert!(overrides.effective_strict("any.path.here", true));
        assert!(!overrides.effective_strict("any.path.here", false));
    }

    #[test]
    fn cascade_uses_nearest_ancestor() {
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("a", true);
        overrides.insert("a.b", false);
        // Unknown key at "a.b.c": parent is "a.b" — explicit false wins.
        assert!(!overrides.effective_strict("a.b.c", true));
        // Unknown key at "a.x": parent is "a" — explicit true wins.
        assert!(overrides.effective_strict("a.x", false));
    }

    #[test]
    fn descendant_can_re_tighten() {
        // The "the first descendant that sets its own strict becomes the
        // new root" test from the proposal.
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("plugins", false);
        overrides.insert("plugins.audit", true);
        // Lenient subtree under `plugins`:
        assert!(!overrides.effective_strict("plugins.foo.bar", true));
        // Re-tightened under `plugins.audit`:
        assert!(overrides.effective_strict("plugins.audit.x", false));
    }

    #[test]
    fn root_override_applies_when_no_more_specific() {
        let mut overrides = StrictnessOverrides::new();
        overrides.insert("", false);
        assert!(!overrides.effective_strict("anything", true));
    }
}
