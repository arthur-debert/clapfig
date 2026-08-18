//! The shared edit path-walkers behind every adapter's
//! [`edit`](super::FormatAdapter::edit).
//!
//! All three adapters edit by the same walk: split the path into parents
//! and leaf, descend the parents creating missing intermediate containers,
//! refuse (typed) when a non-container value already sits where the path
//! needs a container — schema-time pre-checks only validate the schema,
//! not the shape of an existing on-disk file (`config set database.url x`
//! with `database = "string"` already in the file) — then assign or remove
//! at the leaf. The walk lives here ONCE, generic over [`EditDoc`]: each
//! adapter's document tree type (`toml_edit::Item`, the owned
//! [`Value`](crate::value::Value) tree YAML patches against,
//! `serde_json::Value`) implements the seam and keeps only its
//! format-specific node operations.

use super::FormatError;

/// One format's editable document tree, as the shared walkers see it:
/// nested string-keyed containers with format-specific leaf values.
pub(crate) trait EditDoc {
    /// The value type a set edit assigns at the leaf.
    type Value;

    /// The format name carried by `path conflict` errors.
    const FORMAT: &'static str;
    /// The container noun in conflict messages (`"table"`/`"map"`/`"object"`).
    const CONTAINER: &'static str;
    /// The container noun with its article (`"a table"`/`"an object"`).
    const CONTAINER_WITH_ARTICLE: &'static str;
    /// What conflict messages call the edited source (`"file"`/`"document"`).
    const SOURCE: &'static str;

    /// Whether this node is a container that can hold keyed children.
    fn is_container(&self) -> bool;
    /// Whether `key` exists directly under this node.
    fn has_child(&self, key: &str) -> bool;
    /// The child at `key`, whatever its shape.
    fn child_mut(&mut self, key: &str) -> Option<&mut Self>;
    /// Insert an empty container at `key`. Callers guarantee `self` is a
    /// container.
    fn insert_container(&mut self, key: &str);
    /// Assign `value` at `key`. Callers guarantee `self` is a container.
    fn insert_value(&mut self, key: &str, value: Self::Value);
    /// Remove `key`; `false` when nothing was removed (key missing, or
    /// `self` not a container).
    fn remove_key(&mut self, key: &str) -> bool;
}

/// A typed path-conflict edit error for `D`'s format.
fn conflict<D: EditDoc>(message: String) -> FormatError {
    FormatError::Edit {
        format: D::FORMAT,
        message,
    }
}

/// Walk `keys` through `doc`, creating intermediate containers when
/// missing, and assign `value` at the leaf. Errors when an existing
/// intermediate is a non-container value.
pub(crate) fn write_at_path<D: EditDoc>(
    doc: &mut D,
    keys: &[&str],
    value: D::Value,
) -> Result<(), FormatError> {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let display_path = keys.join(".");
    let mut current: &mut D = doc;
    for segment in parents {
        if !current.is_container() {
            return Err(conflict::<D>(format!(
                "path conflict: existing {} has a non-{} value at the path before '{segment}' (setting '{display_path}')",
                D::SOURCE,
                D::CONTAINER
            )));
        }
        if !current.has_child(segment) {
            current.insert_container(segment);
        }
        let next = current
            .child_mut(segment)
            .expect("just confirmed or inserted above");
        if !next.is_container() {
            return Err(conflict::<D>(format!(
                "path conflict: existing {} has a non-{} value at '{segment}' (setting '{display_path}')",
                D::SOURCE,
                D::CONTAINER
            )));
        }
        current = next;
    }
    if !current.is_container() {
        return Err(conflict::<D>(format!(
            "path conflict: leaf parent is not {} (setting '{display_path}')",
            D::CONTAINER_WITH_ARTICLE
        )));
    }
    current.insert_value(leaf, value);
    Ok(())
}

/// Remove the key at `keys`; `false` when the path was already absent (a
/// missing parent or leaf is a no-op — the document is left unchanged).
pub(crate) fn unset_at_path<D: EditDoc>(doc: &mut D, keys: &[&str]) -> bool {
    let (leaf, parents) = keys
        .split_last()
        .expect("ConfigPath edits always carry at least one segment");
    let mut current: &mut D = doc;
    for segment in parents {
        match current.child_mut(segment) {
            Some(next) => current = next,
            None => return false, // parent doesn't exist, nothing to unset
        }
    }
    current.remove_key(leaf)
}

#[cfg(test)]
mod tests {
    // The walkers run against the owned-model tree (the YAML adapter's
    // `EditDoc` impl); each adapter's own tests cover its document type
    // and format-specific conflict wording.
    use super::*;
    use crate::value::{Map, Value};

    #[test]
    fn write_creates_intermediate_containers() {
        let mut root = Value::Map(Map::new());
        write_at_path(&mut root, &["a", "b", "c"], Value::Integer(1)).unwrap();
        let a = root.as_map().unwrap()["a"].as_map().unwrap();
        assert_eq!(a["b"].as_map().unwrap()["c"], Value::Integer(1));
    }

    #[test]
    fn write_conflict_on_non_container_intermediate_is_typed() {
        let mut map = Map::new();
        map.insert("a".into(), Value::Integer(1));
        let mut root = Value::Map(map);
        let err = write_at_path(&mut root, &["a", "b"], Value::Integer(2)).unwrap_err();
        match err {
            FormatError::Edit { format, message } => {
                assert_eq!(format, "yaml");
                assert_eq!(
                    message,
                    "path conflict: existing file has a non-map value at 'a' (setting 'a.b')"
                );
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn unset_reports_whether_anything_was_removed() {
        let mut map = Map::new();
        map.insert("a".into(), Value::Integer(1));
        let mut root = Value::Map(map);
        assert!(!unset_at_path(&mut root, &["missing", "deep"]));
        assert!(!unset_at_path(&mut root, &["a", "not-a-map"]));
        assert!(unset_at_path(&mut root, &["a"]));
        assert!(root.as_map().unwrap().is_empty());
    }
}
