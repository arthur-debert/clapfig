//! Config persistence: patch values into config files while preserving
//! formatting.
//!
//! All source-text mechanics route through the format adapter contract
//! ([`FormatAdapter::edit`]) — for TOML that is lossless
//! comment-preserving editing. This module owns the format-agnostic half:
//! key/type validation against the schema, template seeding for missing
//! files, classifying each set request onto its capability-matrix row
//! ([`SetTarget`] — replace vs create-key vs create-file, so refusals
//! name the operation actually attempted), emitting `debug` persist events
//! (path and key; never the assigned value), key-spelling resolution under
//! [`normalize_keys`](crate::Builder::normalize_keys) (dash and
//! underscore spellings are equivalent; the spelling already present in
//! the document is the one edited, and a document holding both
//! equivalent spellings anywhere errors as a collision rather than one
//! spelling silently winning), and the file I/O around each edit.

use std::path::Path;

use crate::error::ClapfigError;
use crate::format::{ConfigPath, FileEdit, FormatAdapter, Operation, SetTarget};
use crate::normalize::{
    KeyCollision, check_collisions, kebab_key, normalize_key, resolve_table_key,
};
use crate::ops::ConfigResult;
use crate::value::Value;

/// Pure function: patch a config document string, setting `key` to
/// `raw_value`, through `adapter`.
///
/// Validates the key against an owned [`Shape`](crate::runtime::Shape)
/// and parses the raw value **according to the leaf's declared
/// [`LeafType`](crate::runtime::LeafType)** (see [`parse_raw_value`]) —
/// with schema-driven datetime coercion for `DateTime` leaves (ADR-0001)
/// — so a typo in the key name or a value the leaf type cannot accept
/// fails, naming the expected type, before the file is touched. A key
/// addressing a [`Shape::Array`](crate::runtime::Shape::Array) /
/// [`Shape::Map`](crate::runtime::Shape::Map) of objects, a root Map
/// entry, or a path inside one, or a variant-specific / structurally
/// conflicting field of a [`Shape::Tagged`](crate::runtime::Shape::Tagged)
/// union, fails with the targeted
/// [`ClapfigError::UnaddressableKey`] instead of a bare key-not-found:
/// dotted CLI keys cannot index into maps/arrays, and a tagged-union
/// key is refused when the current selection does not address it, so
/// the config file (or selecting a variant that declares the key) is
/// required to edit those sections.
///
/// With `normalize_keys`, the action key follows the load path's
/// acceptance: dash and underscore spellings are equivalent. The key is
/// normalized to the canonical snake_case path before schema validation,
/// paths already present in the document are resolved by that equivalence
/// and edited under the spelling actually present (so setting `pool_size`
/// against a kebab-case document edits `pool-size` instead of creating a
/// colliding sibling), and paths not present are emitted kebab-case —
/// matching what `config gen` emits. A document already holding BOTH
/// equivalent spellings ANYWHERE — even at a key or table the edit never
/// touches — is ambiguous and fails to load, so the edit fails with
/// [`ClapfigError::NormalizedKeyCollision`] instead of editing a
/// document the load path refuses (the I/O wrapper stamps the file path;
/// from this pure function the error's path is empty).
///
/// If `content` is `None` (file doesn't exist yet), starts from the
/// adapter's generated template — rendered with the same `normalize_keys`
/// setting, so the seeded file and `config gen` agree on key spelling —
/// so the new file carries doc comments.
///
/// The edit request carries the capability-matrix row actually attempted
/// ([`SetTarget`]): a missing file is [`Operation::EditCreateFile`]
/// (required up-front, before template seeding), and an existing document
/// is parsed to classify replace ([`Operation::EditSet`]) vs create-key
/// ([`Operation::EditCreateKey`]) — so typed refusals name the operation
/// the caller attempted, not a blanket "set". Parsing is the edit's own
/// first step, so a document the adapter cannot parse fails as its parse
/// error.
///
/// Schema/key validation failures are [`ClapfigError::KeyNotFound`] /
/// [`ClapfigError::UnaddressableKey`] / [`ClapfigError::InvalidValue`];
/// adapter edit failures — including the
/// typed [`UnsupportedByFormat`](crate::format::UnsupportedByFormat)
/// refusal and path conflicts — propagate as [`ClapfigError::Format`],
/// preserving the full [`FormatError`](crate::format::FormatError).
///
/// Returns the modified document string.
pub fn set_in_document(
    adapter: &dyn FormatAdapter,
    shape: &crate::runtime::Shape,
    content: Option<&str>,
    key: &str,
    raw_value: &str,
    normalize_keys: bool,
) -> Result<String, ClapfigError> {
    let canonical = canonical_key(key, normalize_keys);
    let valid_keys = crate::overrides::valid_keys_shape(shape);
    if !valid_keys.contains(&canonical) {
        if let Some((section, kind)) = unaddressable_container_shape(shape, &canonical) {
            return Err(ClapfigError::UnaddressableKey {
                key: key.into(),
                section,
                kind,
            });
        }
        return Err(ClapfigError::KeyNotFound {
            key: key.into(),
            suggestion: crate::meta::nearest_key_shape(shape, &canonical, normalize_keys),
        });
    }

    // Parse an existing document before typed lookup so a tagged
    // object's selected discriminator can resolve variant fields.
    // Classification reuses the same tree.
    let parsed = match content {
        Some(c) => Some(adapter.parse(c).map_err(ClapfigError::from)?),
        None => None,
    };
    // `persist_table_get` uses `resolve_table_key`, whose contract is
    // that the whole document has already been collision-checked. Do
    // that here — before tagged branch selection — so two equivalent
    // discriminator spellings fail as `NormalizedKeyCollision` rather
    // than one spelling silently selecting a variant.
    if normalize_keys && let Some(Value::Map(map)) = parsed.as_ref().map(|p| &p.value) {
        check_collisions(map).map_err(|c| c.into_error(Path::new("")))?;
    }
    let existing = parsed.as_ref().map(|p| &p.value);

    let disc_shape;
    let field = match persist_target(shape, &canonical, existing, normalize_keys) {
        PersistTarget::Shape(s) => Some(s),
        PersistTarget::Discriminator(tagged) => {
            disc_shape = crate::runtime::Shape::leaf(tagged.discriminator_leaf_type());
            Some(&disc_shape)
        }
        PersistTarget::Unaddressable { section, kind } => {
            return Err(ClapfigError::UnaddressableKey {
                key: key.into(),
                section,
                kind,
            });
        }
        PersistTarget::Missing => {
            return Err(ClapfigError::KeyNotFound {
                key: key.into(),
                suggestion: crate::meta::nearest_key_shape(shape, &canonical, normalize_keys),
            });
        }
    };
    let mut value = parse_raw_value(raw_value, field)
        .map_err(|reason| ClapfigError::invalid_value(key, reason))?;
    if let Some(shape) = field {
        crate::schema_walk::coerce_value(&mut value, shape);
        shape
            .check_value(&value)
            .map_err(|reason| ClapfigError::invalid_value(key, reason))?;
    }

    let (base, target, path) = match (content, parsed) {
        (Some(c), Some(parsed)) => {
            // Replace vs create-key depends on whether the path already
            // resolves; classification uses the document parsed above.
            let (segments, exists) =
                resolve_document_path(&parsed.value, &canonical, normalize_keys)
                    .map_err(|c| c.into_error(Path::new("")))?;
            let target = if exists {
                SetTarget::ExistingValue
            } else {
                SetTarget::MissingKey
            };
            (c.to_string(), target, config_path(&segments))
        }
        _ => {
            // Missing file: require the matrix row before template
            // seeding, so the refusal names the attempted operation
            // rather than template generation.
            adapter
                .require(Operation::EditCreateFile)
                .map_err(crate::format::FormatError::from)
                .map_err(ClapfigError::from)?;
            let seeded = crate::ops::generate_template(adapter, shape, normalize_keys)?;
            // The seeded template spells every key the way the enabled
            // normalization emits (kebab-case when on), so the edit path
            // uses that emitted spelling and lands on the template's own
            // keys instead of creating colliding siblings.
            let segments: Vec<String> = canonical
                .split('.')
                .map(|seg| emitted_spelling(seg, normalize_keys))
                .collect();
            (seeded, SetTarget::MissingFile, config_path(&segments))
        }
    };
    let base = if base.trim().is_empty() {
        String::new()
    } else {
        base
    };

    adapter
        .edit(
            &base,
            FileEdit::Set {
                path: &path,
                value: &value,
                target,
            },
        )
        .map_err(ClapfigError::from)
}

/// Stamp `file_path` onto a collision error raised by a document-level
/// (pure, pathless) persist function, so the reported error names the
/// file actually edited — the same shape the load path reports.
fn stamp_collision_path(err: ClapfigError, file_path: &Path) -> ClapfigError {
    match err {
        ClapfigError::NormalizedKeyCollision {
            path,
            section,
            normalized_key,
            originals,
        } if path.as_os_str().is_empty() => ClapfigError::NormalizedKeyCollision {
            path: file_path.to_path_buf(),
            section,
            normalized_key,
            originals,
        },
        other => other,
    }
}

/// Wrapper around [`set_in_document`] with file I/O: reads the file
/// (if it exists), patches it, writes back. Creates parent directories if
/// needed. Collision errors from the document layer get this file's path.
/// A successful write emits a `debug` persist event naming the file and
/// key, never the assigned value.
pub fn persist_value(
    adapter: &dyn FormatAdapter,
    shape: &crate::runtime::Shape,
    file_path: &Path,
    key: &str,
    value: &str,
    normalize_keys: bool,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let new_content = set_in_document(
        adapter,
        shape,
        content.as_deref(),
        key,
        value,
        normalize_keys,
    )
    .map_err(|e| stamp_collision_path(e, file_path))?;

    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ClapfigError::IoError {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    crate::trace::persist_set(file_path, key);
    Ok(ConfigResult::value_set(adapter, key.into(), value.into()))
}

/// Typed persist target for a canonical dotted key.
enum PersistTarget<'a> {
    /// A declared value-shaped field.
    Shape(&'a crate::runtime::Shape),
    /// The synthetic tag leaf of a tagged union (closed discriminator enum).
    Discriminator(&'a crate::runtime::TaggedShape),
    /// Variant-specific or conflicting field with no unique type, or a
    /// selected variant that does not own the key.
    Unaddressable { section: String, kind: &'static str },
    /// Path does not resolve (should not happen after `valid_keys`).
    Missing,
}

/// Pure function: remove a key from a config document string through
/// `adapter`.
///
/// If the key doesn't exist, returns the document unchanged.
/// Navigates dotted key paths (e.g. `"database.pool_size"`). With
/// `normalize_keys`, the document is parsed first and the key is resolved
/// by dash/underscore equivalence, so `unset pool_size` removes an
/// existing `pool-size` entry (and vice versa) — parse failures propagate
/// in that mode, and a document holding both equivalent spellings
/// anywhere fails with [`ClapfigError::NormalizedKeyCollision`] (empty
/// path here; the I/O wrapper stamps the file) rather than editing a
/// document the load path refuses. Comment preservation is per the
/// adapter's declared edit capability; adapter failures propagate as
/// [`ClapfigError::Format`].
///
/// Returns the modified document string.
pub fn unset_in_document(
    adapter: &dyn FormatAdapter,
    content: &str,
    key: &str,
    normalize_keys: bool,
) -> Result<String, ClapfigError> {
    let path = if normalize_keys {
        let tree = adapter.parse(content).map_err(ClapfigError::from)?.value;
        let (segments, _) = resolve_document_path(&tree, &normalize_key(key), true)
            .map_err(|c| c.into_error(Path::new("")))?;
        config_path(&segments)
    } else {
        dotted_config_path(key)
    };
    adapter
        .edit(content, FileEdit::Unset { path: &path })
        .map_err(ClapfigError::from)
}

/// I/O wrapper: reads file, removes the key, writes back.
/// If the file doesn't exist, succeeds silently (nothing to unset).
/// A successful unset (including the missing-file no-op) emits a `debug`
/// persist event naming the file and key.
pub fn unset_value(
    adapter: &dyn FormatAdapter,
    file_path: &Path,
    key: &str,
    normalize_keys: bool,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::trace::persist_unset(file_path, key);
            return Ok(ConfigResult::ValueUnset { key: key.into() });
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let new_content = unset_in_document(adapter, &content, key, normalize_keys)
        .map_err(|e| stamp_collision_path(e, file_path))?;

    std::fs::write(file_path, &new_content).map_err(|e| ClapfigError::IoError {
        path: file_path.to_path_buf(),
        source: e,
    })?;

    crate::trace::persist_unset(file_path, key);
    Ok(ConfigResult::ValueUnset { key: key.into() })
}

/// The canonical snake_case form of a user-supplied action key: with
/// `normalize_keys`, dashes rewrite to underscores (the schema's spelling);
/// without it, the key passes through untouched (exact spelling required,
/// matching the load path's acceptance).
fn canonical_key(key: &str, normalize_keys: bool) -> String {
    if normalize_keys {
        normalize_key(key)
    } else {
        key.to_owned()
    }
}

/// The spelling emitted into a document for a path segment that is not
/// already present: kebab-case when normalization is enabled (matching
/// `config gen` output), the canonical snake_case otherwise.
fn emitted_spelling(segment: &str, normalize_keys: bool) -> String {
    if normalize_keys {
        kebab_key(segment)
    } else {
        segment.to_owned()
    }
}

/// Resolve a canonical snake_case dotted path against a parsed document
/// tree: the concrete per-segment spellings an edit must target, plus
/// whether the full path already resolves to a value — the
/// [`SetTarget::ExistingValue`] vs [`SetTarget::MissingKey`]
/// classification for a set against an existing file.
///
/// With `normalize_keys`, the WHOLE document tree is first validated
/// with [`check_collisions`]: any table holding more than one equivalent
/// spelling (`pool-size` AND `pool_size`) — even one the requested path
/// never traverses — is ambiguous and errors with [`KeyCollision`]. The
/// same never-silently-compete rule the load path enforces, so an edit
/// can never touch a document the load path refuses. Each segment then
/// matches an existing key by dash/underscore equivalence
/// ([`resolve_table_key`]); segments with no match resolve to their
/// emitted (kebab-case) spelling. Without normalization, matching is
/// exact and missing segments keep their canonical spelling.
fn resolve_document_path(
    tree: &Value,
    canonical: &str,
    normalize_keys: bool,
) -> Result<(Vec<String>, bool), KeyCollision> {
    if normalize_keys && let Value::Map(map) = tree {
        check_collisions(map)?;
    }
    let mut segments: Vec<String> = Vec::new();
    let mut current = Some(tree);
    let mut exists = true;
    for seg in canonical.split('.') {
        let matched = match current {
            Some(Value::Map(map)) => {
                if normalize_keys {
                    resolve_table_key(map, seg).cloned()
                } else if map.contains_key(seg) {
                    Some(seg.to_owned())
                } else {
                    None
                }
            }
            _ => None,
        };
        match matched {
            Some(spelling) => {
                current = match current {
                    Some(Value::Map(map)) => map.get(&spelling),
                    _ => None,
                };
                segments.push(spelling);
            }
            None => {
                exists = false;
                current = None;
                segments.push(emitted_spelling(seg, normalize_keys));
            }
        }
    }
    Ok((segments, exists))
}

/// Build a structured [`ConfigPath`] from already-resolved path segments.
fn config_path(segments: &[String]) -> ConfigPath {
    let mut path = ConfigPath::new();
    for segment in segments {
        path = path.key(segment);
    }
    path
}

/// Build a structured [`ConfigPath`] from a dotted persist key. Persist
/// keys are schema-validated dotted paths, so every `.` is a nesting
/// separator (schema field names cannot contain dots).
fn dotted_config_path(key: &str) -> ConfigPath {
    let mut path = ConfigPath::new();
    for segment in key.split('.') {
        path = path.key(segment);
    }
    path
}

/// If the canonical dotted key targets a
/// [`Shape::Array`](crate::runtime::Shape::Array) /
/// [`Shape::Map`](crate::runtime::Shape::Map) of objects (or a root map)
/// or a path inside one,
/// return that section's dotted path and a kind
/// label (`"an array"` / `"a map"`) for [`ClapfigError::UnaddressableKey`].
/// `None` means the key misses the schema some other way (a plain
/// key-not-found). Tagged-union refusals that depend on the document's
/// discriminator are classified later by [`persist_target`].
fn unaddressable_container_shape(
    shape: &crate::runtime::Shape,
    canonical: &str,
) -> Option<(String, &'static str)> {
    match shape {
        crate::runtime::Shape::Object(schema) => unaddressable_container(schema, canonical),
        crate::runtime::Shape::Map(map) => {
            // Any persist key against a root map is a dynamic entry (or a
            // path inside one). Same refuse as a named Map field.
            Some((root_map_section_label(map), "a map"))
        }
        crate::runtime::Shape::Tagged(tagged) => unaddressable_in_tagged(tagged, canonical, ""),
        crate::runtime::Shape::Leaf(_) | crate::runtime::Shape::Array(_) => None,
    }
}

fn root_map_section_label(map: &crate::runtime::MapShape) -> String {
    if map.name.is_empty() {
        "(root)".into()
    } else {
        map.name.clone()
    }
}

fn persist_target<'a>(
    shape: &'a crate::runtime::Shape,
    dotted: &str,
    existing: Option<&Value>,
    normalize_keys: bool,
) -> PersistTarget<'a> {
    match shape {
        crate::runtime::Shape::Object(schema) => persist_target_schema(
            schema,
            dotted,
            existing.and_then(Value::as_map),
            "",
            normalize_keys,
        ),
        crate::runtime::Shape::Tagged(tagged) => persist_target_tagged(
            tagged,
            dotted,
            existing.and_then(Value::as_map),
            "",
            normalize_keys,
        ),
        crate::runtime::Shape::Map(_)
        | crate::runtime::Shape::Leaf(_)
        | crate::runtime::Shape::Array(_) => PersistTarget::Missing,
    }
}

fn unaddressable_container(
    schema: &crate::runtime::Schema,
    canonical: &str,
) -> Option<(String, &'static str)> {
    unaddressable_in_schema(schema, canonical, "")
}

fn unaddressable_in_schema(
    schema: &crate::runtime::Schema,
    canonical: &str,
    prefix: &str,
) -> Option<(String, &'static str)> {
    let (head, rest) = split_first(canonical)?;
    let nf = schema.fields.iter().find(|f| f.name == head)?;
    unaddressable_in_shape(&nf.field, rest, &join_prefix(prefix, head))
}

fn unaddressable_in_shape(
    shape: &crate::runtime::Shape,
    rest: &str,
    walked: &str,
) -> Option<(String, &'static str)> {
    match shape {
        crate::runtime::Shape::Array(array) if !array.item.is_value_field() => {
            Some((walked.to_string(), "an array"))
        }
        crate::runtime::Shape::Map(map) if !map.item.is_value_field() => {
            Some((walked.to_string(), "a map"))
        }
        crate::runtime::Shape::Object(inner) => unaddressable_in_schema(inner, rest, walked),
        crate::runtime::Shape::Tagged(tagged) => unaddressable_in_tagged(tagged, rest, walked),
        crate::runtime::Shape::Leaf(_)
        | crate::runtime::Shape::Array(_)
        | crate::runtime::Shape::Map(_) => None,
    }
}

fn unaddressable_in_tagged(
    tagged: &crate::runtime::TaggedShape,
    rest: &str,
    prefix: &str,
) -> Option<(String, &'static str)> {
    if rest.is_empty() {
        return None;
    }
    let (head, remaining) = split_first(rest)?;
    match tagged.resolve_key(head) {
        crate::runtime::KeyAcrossVariants::Tag | crate::runtime::KeyAcrossVariants::Absent => None,
        crate::runtime::KeyAcrossVariants::Every(shapes)
        | crate::runtime::KeyAcrossVariants::Partial(shapes) => {
            let walked = join_prefix(prefix, head);
            shapes
                .into_iter()
                .find_map(|shape| unaddressable_in_shape(shape, remaining, &walked))
        }
    }
}

fn persist_target_schema<'a>(
    schema: &'a crate::runtime::Schema,
    dotted: &str,
    table: Option<&crate::value::Map>,
    prefix: &str,
    normalize_keys: bool,
) -> PersistTarget<'a> {
    let Some((head, rest)) = split_first(dotted) else {
        return PersistTarget::Missing;
    };
    let Some(nf) = schema.fields.iter().find(|f| f.name == head) else {
        return PersistTarget::Missing;
    };
    persist_target_in_shape(
        &nf.field,
        rest,
        table.and_then(|t| persist_table_get(t, head, normalize_keys)),
        &join_prefix(prefix, head),
        normalize_keys,
    )
}

fn persist_target_in_shape<'a>(
    shape: &'a crate::runtime::Shape,
    rest: &str,
    value: Option<&Value>,
    walked: &str,
    normalize_keys: bool,
) -> PersistTarget<'a> {
    if rest.is_empty() {
        return if shape.is_value_field() {
            PersistTarget::Shape(shape)
        } else {
            PersistTarget::Missing
        };
    }
    match shape {
        crate::runtime::Shape::Object(inner) => persist_target_schema(
            inner,
            rest,
            value.and_then(Value::as_map),
            walked,
            normalize_keys,
        ),
        crate::runtime::Shape::Tagged(tagged) => persist_target_tagged(
            tagged,
            rest,
            value.and_then(Value::as_map),
            walked,
            normalize_keys,
        ),
        _ => PersistTarget::Missing,
    }
}

fn persist_target_tagged<'a>(
    tagged: &'a crate::runtime::TaggedShape,
    dotted: &str,
    table: Option<&crate::value::Map>,
    prefix: &str,
    normalize_keys: bool,
) -> PersistTarget<'a> {
    let Some((head, rest)) = split_first(dotted) else {
        return PersistTarget::Missing;
    };
    let section = tagged_section(prefix, tagged);
    match tagged.resolve_key(head) {
        crate::runtime::KeyAcrossVariants::Tag => {
            if rest.is_empty() {
                PersistTarget::Discriminator(tagged)
            } else {
                PersistTarget::Missing
            }
        }
        classification => {
            if let Some(variant) =
                table.and_then(|t| persist_selected_variant(tagged, t, normalize_keys))
            {
                if variant.schema.fields.iter().all(|f| f.name != head) {
                    return PersistTarget::Unaddressable {
                        section,
                        kind: "a tagged union",
                    };
                }
                return persist_target_schema(
                    &variant.schema,
                    dotted,
                    table,
                    prefix,
                    normalize_keys,
                );
            }
            let declared = match classification {
                crate::runtime::KeyAcrossVariants::Tag => {
                    unreachable!("tag handled above")
                }
                crate::runtime::KeyAcrossVariants::Absent => {
                    return PersistTarget::Missing;
                }
                crate::runtime::KeyAcrossVariants::Partial(_) => {
                    // A field missing from some variants is variant-specific:
                    // refuse until a discriminator selects a branch (spec:
                    // targeted refuse).
                    return PersistTarget::Unaddressable {
                        section,
                        kind: "a tagged union",
                    };
                }
                crate::runtime::KeyAcrossVariants::Every(shapes) => shapes,
            };
            let walked = join_prefix(prefix, head);
            let child = table.and_then(|t| persist_table_get(t, head, normalize_keys));
            if rest.is_empty() {
                return unambiguous_value_shapes(declared, section);
            }
            let mut agreed: Option<PersistTarget<'a>> = None;
            for shape in declared {
                let next = persist_target_in_shape(shape, rest, child, &walked, normalize_keys);
                agreed = Some(match (agreed, next) {
                    (None, t) => t,
                    (Some(PersistTarget::Shape(a)), PersistTarget::Shape(b))
                        if a.structurally_agrees_with(b) =>
                    {
                        PersistTarget::Shape(a)
                    }
                    (Some(PersistTarget::Discriminator(a)), PersistTarget::Discriminator(b))
                        if a.tag == b.tag =>
                    {
                        PersistTarget::Discriminator(a)
                    }
                    _ => {
                        return PersistTarget::Unaddressable {
                            section,
                            kind: "a tagged union",
                        };
                    }
                });
            }
            agreed.unwrap_or(PersistTarget::Missing)
        }
    }
}

/// Look up a schema field name in a raw persist document. With
/// `normalize_keys`, dash and underscore spellings are equivalent — the
/// same rule [`resolve_document_path`] uses — so a kebab-case tagged
/// document still selects the variant for `block_kind` stored as
/// `block-kind`.
///
/// Callers must already have run [`check_collisions`] on the document
/// (see [`set_in_document`]); this only chooses the concrete spelling.
fn persist_table_get<'a>(
    table: &'a crate::value::Map,
    schema_key: &str,
    normalize_keys: bool,
) -> Option<&'a Value> {
    if normalize_keys {
        resolve_table_key(table, schema_key).and_then(|k| table.get(k))
    } else {
        table.get(schema_key)
    }
}

fn persist_selected_variant<'a>(
    tagged: &'a crate::runtime::TaggedShape,
    table: &crate::value::Map,
    normalize_keys: bool,
) -> Option<&'a crate::runtime::TaggedVariant> {
    match persist_table_get(table, &tagged.tag, normalize_keys) {
        Some(Value::String(name)) => tagged.variant(name),
        _ => None,
    }
}

fn unambiguous_value_shapes<'a>(
    declared: Vec<&'a crate::runtime::Shape>,
    section: String,
) -> PersistTarget<'a> {
    if !declared.iter().all(|s| s.is_value_field()) {
        return PersistTarget::Unaddressable {
            section,
            kind: "a tagged union",
        };
    }
    let first = declared[0];
    if declared
        .iter()
        .skip(1)
        .all(|s| first.structurally_agrees_with(s))
    {
        PersistTarget::Shape(first)
    } else {
        PersistTarget::Unaddressable {
            section,
            kind: "a tagged union",
        }
    }
}

fn tagged_section(prefix: &str, tagged: &crate::runtime::TaggedShape) -> String {
    if !prefix.is_empty() {
        prefix.to_string()
    } else if !tagged.name.is_empty() {
        tagged.name.clone()
    } else {
        tagged.tag.clone()
    }
}

fn join_prefix(prefix: &str, head: &str) -> String {
    if prefix.is_empty() {
        head.to_string()
    } else {
        format!("{prefix}.{head}")
    }
}

fn split_first(dotted: &str) -> Option<(&str, &str)> {
    if dotted.is_empty() {
        return None;
    }
    match dotted.find('.') {
        Some(i) => Some((&dotted[..i], &dotted[i + 1..])),
        None => Some((dotted, "")),
    }
}

/// Parse a raw `config set` string into a typed config value **according
/// to the target leaf's declared [`LeafType`](crate::runtime::LeafType)**
/// — never by sniffing the string's shape when the schema already says
/// what the leaf holds:
///
/// - `String` takes the raw string verbatim, so values that *look* like
///   numbers or bools (`"123"`, `"true"`) are settable on string leaves.
/// - `Integer` / `Float` / `Bool` parse the raw string as that type and
///   fail naming the expected type.
/// - `Array` / `Map` parse the raw string as a TOML inline value (`[1,
///   2]`, `{a = 1}` — the value model's baseline vocabulary, whatever
///   the file format), then the caller's [`LeafType::check`] validates
///   element types.
/// - `DateTime` passes the raw string through; the caller's
///   schema-driven coercion (ADR-0001) plus `check` accept or reject it.
/// - `Enum` uses the env-style bool > integer > float > string heuristic
///   (its members carry their own types), falling back to the raw string
///   so string members that look numeric stay reachable; membership is
///   the caller's `check`.
/// - `Value` leaves (and keys without a resolvable leaf type) keep the
///   env-style heuristic — the schema declares no shape to parse toward.
///
/// Errors are human-readable reasons for
/// [`ClapfigError::InvalidValue`](crate::error::ClapfigError::InvalidValue).
fn parse_raw_value(raw: &str, shape: Option<&crate::runtime::Shape>) -> Result<Value, String> {
    use crate::runtime::{LeafType, Shape};
    let Some(shape) = shape else {
        return Ok(crate::env::parse_env_value(raw));
    };
    match shape {
        Shape::Array(_) => parse_inline_container(raw, "array", "[\"a\", \"b\"]"),
        Shape::Map(_) => parse_inline_container(raw, "map", "{key = \"value\"}"),
        Shape::Leaf(leaf) => match &leaf.ty {
            LeafType::String => Ok(Value::String(raw.to_owned())),
            LeafType::Integer { .. } => raw
                .parse::<i64>()
                .map(Value::Integer)
                .map_err(|_| format!("expected integer, got '{raw}'")),
            LeafType::Float => raw
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| format!("expected float, got '{raw}'")),
            LeafType::Bool => {
                if raw.eq_ignore_ascii_case("true") {
                    Ok(Value::Boolean(true))
                } else if raw.eq_ignore_ascii_case("false") {
                    Ok(Value::Boolean(false))
                } else {
                    Err(format!("expected bool ('true' or 'false'), got '{raw}'"))
                }
            }
            LeafType::DateTime => Ok(Value::String(raw.to_owned())),
            LeafType::Enum { values } => {
                let sniffed = crate::env::parse_env_value(raw);
                if values.contains(&sniffed) {
                    Ok(sniffed)
                } else {
                    Ok(Value::String(raw.to_owned()))
                }
            }
            LeafType::Value => Ok(crate::env::parse_env_value(raw)),
        },
        Shape::Object(_) | Shape::Tagged(_) => Ok(crate::env::parse_env_value(raw)),
    }
}

/// Parse a raw `config set` string destined for an `Array`/`Map` field as
/// a TOML inline value. TOML is the value model's baseline vocabulary
/// (ADR-0001), so the CLI accepts one container syntax regardless of the
/// file's format; the resulting [`Value`] is then written through the
/// active format's adapter like any other. A raw string TOML cannot
/// parse as a value errors naming the expected container type with an
/// example spelling.
fn parse_inline_container(raw: &str, kind: &str, example: &str) -> Result<Value, String> {
    let refuse = || format!("expected {kind} in TOML inline syntax (e.g. {example}), got '{raw}'");
    let doc = format!("v = {raw}");
    match crate::format::TomlAdapter.parse(&doc) {
        // Require exactly the probe key back: a raw string smuggling
        // extra TOML (newlines, additional keys) is not one value.
        Ok(parsed) => match parsed.value {
            Value::Map(mut map) if map.len() == 1 => map.remove("v").ok_or_else(refuse),
            _ => Err(refuse()),
        },
        _ => Err(refuse()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::{enum_schema, test_schema};
    use crate::format::TomlAdapter;
    use crate::runtime::Shape;
    use std::fs;
    use tempfile::TempDir;

    fn set_toml(content: Option<&str>, key: &str, value: &str) -> Result<String, ClapfigError> {
        set_in_document(
            &TomlAdapter,
            &Shape::Object(test_schema()),
            content,
            key,
            value,
            false,
        )
    }

    fn persist_toml(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> Result<ConfigResult, ClapfigError> {
        persist_value(
            &TomlAdapter,
            &Shape::Object(test_schema()),
            path,
            key,
            value,
            false,
        )
    }

    // --- validation tests ---

    /// In-test adapter that parses like TOML but declares no edit rows:
    /// set-target classification succeeds, and every edit refuses with
    /// the request's own operation — the shape a partially-implemented
    /// adapter (parse landed, edits not yet) presents.
    struct ParseOnly;

    impl FormatAdapter for ParseOnly {
        fn name(&self) -> &'static str {
            "parseonly"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["po"]
        }

        fn capabilities(&self) -> &'static [crate::format::Operation] {
            &[crate::format::Operation::Parse]
        }

        fn parse(&self, text: &str) -> Result<crate::format::Parsed, crate::format::FormatError> {
            TomlAdapter.parse(text)
        }

        fn serialize(&self, _value: &Value) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Serialize)
                .unwrap_err()
                .into())
        }

        fn template(
            &self,
            _shape: &crate::runtime::Shape,
        ) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Template)
                .unwrap_err()
                .into())
        }

        fn edit(
            &self,
            _source: &str,
            edit: FileEdit<'_>,
        ) -> Result<String, crate::format::FormatError> {
            Err(self.require(edit.operation()).unwrap_err().into())
        }
    }

    /// Unwrap a persist error down to the typed refusal.
    fn refusal(result: Result<String, ClapfigError>) -> crate::format::UnsupportedByFormat {
        match result.unwrap_err() {
            ClapfigError::Format(crate::format::FormatError::Unsupported(u)) => u,
            other => panic!("expected typed refusal, got {other:?}"),
        }
    }

    #[test]
    fn set_refusals_name_the_attempted_matrix_row() {
        // The refusal reports the capability-matrix row the caller
        // actually attempted — replace, create-key, or create-file — not
        // a blanket "replacing an existing value" for every set.
        let schema = test_schema();

        // Key exists in the document → replacing an existing value.
        let u = refusal(set_in_document(
            &ParseOnly,
            &Shape::Object(schema.clone()),
            Some("port = 1\n"),
            "port",
            "2",
            false,
        ));
        assert_eq!(u.operation, Operation::EditSet);

        // File exists, key does not → creating a missing key.
        let u = refusal(set_in_document(
            &ParseOnly,
            &Shape::Object(schema.clone()),
            Some("port = 1\n"),
            "debug",
            "true",
            false,
        ));
        assert_eq!(u.operation, Operation::EditCreateKey);

        // No file at all → creating a missing file, refused before any
        // template seeding (ParseOnly's template also refuses; the
        // create-file refusal must win).
        let u = refusal(set_in_document(
            &ParseOnly,
            &Shape::Object(schema.clone()),
            None,
            "port",
            "1",
            false,
        ));
        assert_eq!(u.operation, Operation::EditCreateFile);
    }

    /// In-test adapter that declares nothing at all — the shape a
    /// not-yet-implemented format presents (every built-in adapter is
    /// implemented now, so the shape only exists in-test).
    struct RefusesAll;

    impl FormatAdapter for RefusesAll {
        fn name(&self) -> &'static str {
            "refusesall"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["ra"]
        }

        fn capabilities(&self) -> &'static [crate::format::Operation] {
            &[]
        }

        fn parse(&self, _text: &str) -> Result<crate::format::Parsed, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Parse)
                .unwrap_err()
                .into())
        }

        fn serialize(&self, _value: &Value) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Serialize)
                .unwrap_err()
                .into())
        }

        fn template(
            &self,
            _shape: &crate::runtime::Shape,
        ) -> Result<String, crate::format::FormatError> {
            Err(self
                .require(crate::format::Operation::Template)
                .unwrap_err()
                .into())
        }

        fn edit(
            &self,
            _source: &str,
            edit: FileEdit<'_>,
        ) -> Result<String, crate::format::FormatError> {
            Err(self.require(edit.operation()).unwrap_err().into())
        }
    }

    #[test]
    fn set_on_unparseable_format_fails_at_parse() {
        // A format that cannot parse at all fails classification at its
        // parse refusal — editing an existing file begins with reading
        // it, so that is the honest earliest error.
        let u = refusal(set_in_document(
            &RefusesAll,
            &Shape::Object(test_schema()),
            Some("port = 1\n"),
            "port",
            "2",
            false,
        ));
        assert_eq!(u.operation, Operation::Parse);
    }

    #[test]
    fn set_rejects_unknown_key() {
        let result = set_toml(Some(""), "nonexistent", "value");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound { .. })));
    }

    #[test]
    fn set_kebab_key_suggests_snake_when_normalization_is_off() {
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(test_schema()),
            Some(""),
            "database.pool-size",
            "10",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::KeyNotFound { key, suggestion } => {
                assert_eq!(key, "database.pool-size");
                assert_eq!(suggestion.as_deref(), Some("database.pool_size"));
            }
            other => panic!("expected KeyNotFound, got {other:?}"),
        }
    }

    #[test]
    fn set_rejects_invalid_enum_value() {
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(enum_schema()),
            Some(""),
            "mode",
            "garbage",
            false,
        );
        match result {
            Err(ClapfigError::InvalidValue { key, reason, .. }) => {
                assert_eq!(key, "mode");
                assert!(
                    reason.contains("not in allowed set"),
                    "expected 'not in allowed set' in: {reason}"
                );
            }
            other => panic!("Expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_accepts_valid_enum_value() {
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(enum_schema()),
            Some(""),
            "mode",
            "fast",
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn set_rejects_wrong_type() {
        let result = set_toml(Some(""), "port", "not_a_number");
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
    }

    #[test]
    fn set_rejects_path_through_scalar() {
        // Existing file has `database` as a scalar string; `config set
        // database.url x` would dereference into a non-table item, which
        // pre-fix would panic inside the TOML editor's IndexMut. The
        // guard turns it into the adapter's typed edit failure, which
        // propagates as ClapfigError::Format (never collapsed into
        // InvalidValue — that variant is for schema/type validation).
        let content = "database = \"oops\"\n";
        let result = set_toml(Some(content), "database.url", "pg://x");
        match result {
            Err(ClapfigError::Format(crate::format::FormatError::Edit {
                format, message, ..
            })) => {
                assert_eq!(format, "toml");
                assert!(message.contains("path conflict"), "got: {message}");
            }
            other => panic!("expected Format(Edit), got {other:?}"),
        }
    }

    #[test]
    fn persist_rejects_invalid_enum_value() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_value(
            &TomlAdapter,
            &Shape::Object(enum_schema()),
            &path,
            "mode",
            "garbage",
            false,
        );
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
        // File should NOT have been created
        assert!(!path.exists());
    }

    #[test]
    fn set_existing_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = set_toml(Some(content), "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn set_nested_key() {
        let content = "[database]\npool_size = 5\n";
        let result = set_toml(Some(content), "database.pool_size", "20").unwrap();
        assert!(result.contains("pool_size = 20"));
    }

    #[test]
    fn set_new_key_in_existing_file() {
        let content = "port = 8080\n";
        let result = set_toml(Some(content), "debug", "true").unwrap();
        assert!(result.contains("debug = true"));
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn set_creates_from_template_when_none() {
        let result = set_toml(None, "port", "3000").unwrap();
        assert!(result.contains("port = 3000"));
    }

    #[test]
    fn preserves_comments() {
        let content = "# This is my config\nport = 8080\n# end\n";
        let result = set_toml(Some(content), "port", "3000").unwrap();
        assert!(result.contains("# This is my config"));
        assert!(result.contains("port = 3000"));
    }

    // --- schema-directed value parsing (the leaf type decides, not a
    // --- sniff of the raw string's shape) ---

    #[test]
    fn value_parsing_follows_declared_leaf_type() {
        use crate::runtime::{Field, Shape};
        // The same raw string lands as different types depending on the
        // declared leaf — never on what the string looks like.
        assert_eq!(
            parse_raw_value("123", Some(&Shape::from(Field::string()))).unwrap(),
            Value::String("123".into())
        );
        assert_eq!(
            parse_raw_value("123", Some(&Shape::from(Field::integer()))).unwrap(),
            Value::Integer(123)
        );
        assert_eq!(
            parse_raw_value("123", Some(&Shape::from(Field::float()))).unwrap(),
            Value::Float(123.0)
        );
        assert_eq!(
            parse_raw_value("TRUE", Some(&Shape::from(Field::boolean()))).unwrap(),
            Value::Boolean(true)
        );
    }

    #[test]
    fn value_parsing_errors_name_the_expected_type() {
        use crate::runtime::{Field, LeafType, Shape};
        for (raw, shape, expected) in [
            ("abc", Shape::from(Field::integer()), "expected integer"),
            ("abc", Shape::from(Field::float()), "expected float"),
            ("yes", Shape::from(Field::boolean()), "expected bool"),
            (
                "a,b",
                Shape::from(Field::array_of_type(LeafType::String)),
                "expected array",
            ),
            (
                "a=1",
                Shape::from(Field::map_of(Field::integer())),
                "expected map",
            ),
        ] {
            let err = parse_raw_value(raw, Some(&shape)).unwrap_err();
            assert!(err.contains(expected), "{raw}: {err}");
            assert!(err.contains(raw), "{raw}: {err}");
        }
    }

    #[test]
    fn value_parsing_without_leaf_type_keeps_the_heuristic() {
        // `Value` leaves and unresolvable keys have no declared shape to
        // parse toward — the env-style sniff stays.
        use crate::runtime::{Field, Shape};
        assert_eq!(parse_raw_value("42", None).unwrap(), Value::Integer(42));
        assert_eq!(
            parse_raw_value("1.5", Some(&Shape::from(Field::value()))).unwrap(),
            Value::Float(1.5)
        );
        assert_eq!(
            parse_raw_value("hello", None).unwrap(),
            Value::String("hello".into())
        );
    }

    #[test]
    fn set_string_leaf_accepts_numeric_looking_value() {
        // The pre-fix failure mode: `config set host 123` on a String
        // leaf sniffed 123 as an integer and refused with "expected
        // string, got integer" — with no way to force a string.
        let result = set_toml(Some(""), "host", "123").unwrap();
        assert!(result.contains("host = \"123\""), "{result}");
    }

    #[test]
    fn set_array_leaf_parses_toml_inline_array() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field(
                "tags",
                Field::array_of_type(crate::runtime::LeafType::String).optional(),
            )
            .build();
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "tags",
            "[\"a\", \"b\"]",
            false,
        )
        .unwrap();
        assert!(result.contains("tags = [\"a\", \"b\"]"), "{result}");

        // Element types are still checked against the declared element.
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "tags",
            "[1, 2]",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::InvalidValue { reason, .. } => {
                assert!(reason.contains("expected string"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_map_leaf_parses_toml_inline_table() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field(
                "limits",
                Field::map_of(crate::runtime::LeafType::Integer {
                    min: None,
                    max: None,
                })
                .optional(),
            )
            .build();
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "limits",
            "{cpu = 2, mem = 8}",
            false,
        )
        .unwrap();
        assert!(result.contains("cpu = 2"), "{result}");
        assert!(result.contains("mem = 8"), "{result}");
    }

    #[test]
    fn set_container_rejects_smuggled_extra_toml() {
        // A raw string that parses as MORE than one value (an injected
        // second key) is not one value — refuse rather than silently
        // keeping the first.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field(
                "tags",
                Field::array_of_type(crate::runtime::LeafType::String).optional(),
            )
            .build();
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "tags",
            "[]\nother = 1",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, ClapfigError::InvalidValue { .. }), "{err:?}");
    }

    #[test]
    fn set_enum_of_numeric_strings_stays_reachable() {
        // An enum whose members are strings that LOOK numeric: the raw
        // value must still match the string member, not die as a sniffed
        // integer.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field("gear", Field::enum_of(["1", "2", "reverse"]).optional())
            .build();
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "gear",
            "1",
            false,
        )
        .unwrap();
        assert!(result.contains("gear = \"1\""), "{result}");
    }

    #[test]
    fn set_enum_of_integers_matches_typed_member() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field("level", Field::enum_of([1i64, 2i64, 3i64]).optional())
            .build();
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "level",
            "2",
            false,
        )
        .unwrap();
        assert!(result.contains("level = 2"), "{result}");
    }

    // --- ArrayOf/MapOf interior addressing (targeted refusal) ---

    /// Schema with an `ArrayOf` and a `MapOf` section plus one nested
    /// object wrapping a `MapOf`, for interior-path refusal tests.
    fn container_schema() -> crate::runtime::Schema {
        use crate::runtime::{Field, Schema};
        Schema::object("T")
            .array_of(
                "plugins",
                Schema::object("Plugin").field("name", Field::string()),
            )
            .map_of(
                "servers",
                Schema::object("Server").field("host", Field::string()),
            )
            .nested(
                "outer",
                Schema::object("Outer").map_of(
                    "inner",
                    Schema::object("Entry").field("port", Field::integer()),
                ),
            )
            .build()
    }

    #[test]
    fn set_into_map_of_interior_names_the_section() {
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(container_schema()),
            Some(""),
            "servers.web.host",
            "example.com",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::UnaddressableKey { key, section, kind } => {
                assert_eq!(key, "servers.web.host");
                assert_eq!(section, "servers");
                assert_eq!(kind, "a map");
            }
            other => panic!("expected UnaddressableKey, got {other:?}"),
        }
    }

    #[test]
    fn set_into_array_of_interior_names_the_section() {
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(container_schema()),
            Some(""),
            "plugins.name",
            "x",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::UnaddressableKey { section, kind, .. } => {
                assert_eq!(section, "plugins");
                assert_eq!(kind, "an array");
            }
            other => panic!("expected UnaddressableKey, got {other:?}"),
        }
    }

    #[test]
    fn set_on_nested_container_section_reports_full_path() {
        // The section path walks through Nested wrappers, and targeting
        // the container field itself (no interior segment) refuses too.
        for key in ["outer.inner.web.port", "outer.inner"] {
            let err = set_in_document(
                &TomlAdapter,
                &Shape::Object(container_schema()),
                Some(""),
                key,
                "1",
                false,
            )
            .unwrap_err();
            match err {
                ClapfigError::UnaddressableKey { section, kind, .. } => {
                    assert_eq!(section, "outer.inner", "{key}");
                    assert_eq!(kind, "a map", "{key}");
                }
                other => panic!("{key}: expected UnaddressableKey, got {other:?}"),
            }
        }
    }

    #[test]
    fn set_unknown_key_off_containers_is_still_key_not_found() {
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(container_schema()),
            Some(""),
            "nonexistent.path",
            "1",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, ClapfigError::KeyNotFound { .. }), "{err:?}");
    }

    #[test]
    fn normalized_set_into_container_interior_refuses_kebab_spelling_too() {
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .map_of(
                "my_servers",
                Schema::object("Server").field("host", Field::string()),
            )
            .build();
        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "my-servers.web.host",
            "h",
            true,
        )
        .unwrap_err();
        match err {
            ClapfigError::UnaddressableKey { section, .. } => assert_eq!(section, "my_servers"),
            other => panic!("expected UnaddressableKey, got {other:?}"),
        }
    }

    #[test]
    fn set_coerces_datetime_string_for_datetime_leaf() {
        // Schema-driven datetime coercion (ADR-0001) applies to `config
        // set` too: the heuristic parses the raw string as a String, and
        // the DateTime leaf declaration coerces it before the type check.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .field("stamp", Field::datetime().optional())
            .build();
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "stamp",
            "2024-01-02T03:04:05Z",
            false,
        )
        .unwrap();
        assert!(
            result.contains("stamp = 2024-01-02T03:04:05Z"),
            "datetime must persist unquoted (typed), got: {result}"
        );

        let err = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(""),
            "stamp",
            "not-a-date",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, ClapfigError::InvalidValue { .. }));
    }

    #[test]
    fn persist_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");

        let result = persist_toml(&path, "port", "3000").unwrap();
        assert!(matches!(result, ConfigResult::ValueSet { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
    }

    #[test]
    fn persist_modifies_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\n").unwrap();

        persist_toml(&path, "port", "3000").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("port = 3000"));
        assert!(!content.contains("8080"));
    }

    /// Guard (FIXME.md #13, test dropped by an earlier refactor): an
    /// unreadable target file is an IO error naming the file — never
    /// treated like a missing file, which would silently overwrite it
    /// with a fresh template-seeded document.
    #[cfg(unix)]
    #[test]
    fn persist_unreadable_file_surfaces_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&path).is_ok() {
            // Running as root: permissions don't bind, nothing to test.
            return;
        }

        let result = persist_toml(&path, "port", "3000");
        match result {
            Err(ClapfigError::IoError { path: reported, .. }) => assert_eq!(reported, path),
            other => panic!("expected IoError, got {other:?}"),
        }

        // The unreadable file was not replaced.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "port = 8080\n");
    }

    #[test]
    fn persist_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join("dir").join("config.toml");

        persist_toml(&path, "port", "3000").unwrap();
        assert!(path.exists());
    }

    // --- normalize_keys tests ---
    //
    // The persistence path must match the load path's acceptance: with
    // normalization on, dash and underscore action-key spellings are
    // equivalent, edits land on the spelling the document already uses,
    // and missing paths/files come out kebab-case. Format-agnostic by
    // construction (the logic lives before the adapter seam), so every
    // scenario runs against all three adapters.

    use crate::format::{JsonAdapter, YamlAdapter};

    /// Per-adapter document with the schema's `database.pool_size` leaf
    /// spelled in the given (kebab or snake) form.
    fn docs(leaf: &str) -> [(&'static dyn FormatAdapter, String); 3] {
        [
            (&TomlAdapter, format!("[database]\n{leaf} = 5\n")),
            (&YamlAdapter, format!("database:\n  {leaf}: 5\n")),
            (
                &JsonAdapter,
                format!("{{\n  \"database\": {{\n    \"{leaf}\": 5\n  }}\n}}\n"),
            ),
        ]
    }

    fn doc_map(adapter: &dyn FormatAdapter, text: &str) -> crate::value::Map {
        match adapter.parse(text).unwrap().value {
            Value::Map(m) => m,
            other => panic!("expected map root, got {other:?}"),
        }
    }

    #[test]
    fn normalized_set_edits_kebab_document_for_both_spellings() {
        // The collision scenario from #122: setting the snake key against
        // a kebab document must edit the existing kebab entry, never
        // create a snake sibling (which would NormalizedKeyCollision on
        // the next load). The kebab action key must work too.
        for (adapter, doc) in docs("pool-size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = set_in_document(
                    adapter,
                    &Shape::Object(test_schema()),
                    Some(&doc),
                    action_key,
                    "20",
                    true,
                )
                .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool-size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool_size"),
                    "{} / {action_key} created a colliding snake sibling:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_edits_snake_document_for_both_spellings() {
        // Equivalence is symmetric: a document still spelled snake_case
        // keeps its spelling — the edit targets what is present, it does
        // not rewrite the document to kebab.
        for (adapter, doc) in docs("pool_size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = set_in_document(
                    adapter,
                    &Shape::Object(test_schema()),
                    Some(&doc),
                    action_key,
                    "20",
                    true,
                )
                .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool_size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool-size"),
                    "{} / {action_key} created a colliding kebab sibling:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_emits_kebab_for_missing_paths() {
        // A path not present in the document is created in the emitted
        // (kebab) spelling — the same spelling `config gen` produces.
        let bases: [(&dyn FormatAdapter, &str); 3] = [
            (&TomlAdapter, "host = \"h\"\n"),
            (&YamlAdapter, "host: h\n"),
            (&JsonAdapter, "{\n  \"host\": \"h\"\n}\n"),
        ];
        for (adapter, base) in bases {
            let out = set_in_document(
                adapter,
                &Shape::Object(test_schema()),
                Some(base),
                "database.pool_size",
                "20",
                true,
            )
            .unwrap();
            let map = doc_map(adapter, &out);
            let db = map["database"].as_map().unwrap();
            assert_eq!(
                db.get("pool-size"),
                Some(&Value::Integer(20)),
                "{}:\n{out}",
                adapter.name()
            );
            assert!(!db.contains_key("pool_size"), "{}:\n{out}", adapter.name());
        }
    }

    #[test]
    fn normalized_set_seeds_missing_file_with_normalized_template() {
        // Missing files seed from the template rendered with the SAME
        // normalization setting, so the seeded file and `config gen`
        // agree on key spelling — and the set lands on the template's own
        // kebab key instead of adding a snake duplicate.
        let adapters: [&dyn FormatAdapter; 3] = [&TomlAdapter, &YamlAdapter, &JsonAdapter];
        for adapter in adapters {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = set_in_document(
                    adapter,
                    &Shape::Object(test_schema()),
                    None,
                    action_key,
                    "20",
                    true,
                )
                .unwrap();
                let map = doc_map(adapter, &out);
                let db = map["database"].as_map().unwrap();
                assert_eq!(
                    db.get("pool-size"),
                    Some(&Value::Integer(20)),
                    "{} / {action_key}:\n{out}",
                    adapter.name()
                );
                assert!(
                    !db.contains_key("pool_size"),
                    "{} / {action_key} seeded a snake key:\n{out}",
                    adapter.name()
                );
                assert!(
                    !out.contains("pool_size"),
                    "{} / {action_key} snake spelling leaked into seeded file:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn normalized_set_validates_kebab_key_against_canonical_schema() {
        // The kebab action key is accepted (normalized before the
        // valid-keys check) and its value is still type-checked against
        // the canonical leaf.
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(test_schema()),
            Some(""),
            "database.pool-size",
            "not_a_number",
            true,
        );
        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
    }

    #[test]
    fn kebab_action_key_still_rejected_without_normalization() {
        // Acceptance boundary: with normalization off, the load path
        // rejects kebab keys, so the persistence path does too.
        let result = set_toml(Some(""), "database.pool-size", "20");
        assert!(matches!(result, Err(ClapfigError::KeyNotFound { .. })));
    }

    #[test]
    fn normalized_unset_removes_equivalent_spelling() {
        for (adapter, doc) in docs("pool-size") {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let out = unset_in_document(adapter, &doc, action_key, true).unwrap();
                let map = doc_map(adapter, &out);
                let db = map
                    .get("database")
                    .map(|d| d.as_map().unwrap().clone())
                    .unwrap_or_default();
                assert!(
                    !db.contains_key("pool-size") && !db.contains_key("pool_size"),
                    "{} / {action_key} left the key behind:\n{out}",
                    adapter.name()
                );
            }
        }
    }

    /// Per-adapter document with BOTH spellings of the pool-size leaf —
    /// a file the load path refuses as a NormalizedKeyCollision.
    fn colliding_docs() -> [(&'static dyn FormatAdapter, &'static str); 3] {
        [
            (&TomlAdapter, "[database]\npool-size = 5\npool_size = 6\n"),
            (&YamlAdapter, "database:\n  pool-size: 5\n  pool_size: 6\n"),
            (
                &JsonAdapter,
                "{\n  \"database\": {\n    \"pool-size\": 5,\n    \"pool_size\": 6\n  }\n}\n",
            ),
        ]
    }

    /// Unwrap a persist error down to the collision, asserting its shape.
    fn assert_collision(
        result: Result<String, ClapfigError>,
        section: &str,
        normalized_key: &str,
        originals: &[&str],
        context: &str,
    ) {
        match result.unwrap_err() {
            ClapfigError::NormalizedKeyCollision {
                section: s,
                normalized_key: n,
                originals: o,
                ..
            } => {
                assert_eq!(s, section, "{context}");
                assert_eq!(n, normalized_key, "{context}");
                assert_eq!(o, originals, "{context}");
            }
            other => panic!("{context}: expected NormalizedKeyCollision, got {other:?}"),
        }
    }

    #[test]
    fn normalized_set_rejects_ambiguous_leaf_spellings() {
        // A document holding BOTH equivalent spellings fails to load, so
        // set must refuse with the same collision instead of silently
        // editing one of the two entries and leaving the file unloadable.
        for (adapter, doc) in colliding_docs() {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let result = set_in_document(
                    adapter,
                    &Shape::Object(test_schema()),
                    Some(doc),
                    action_key,
                    "20",
                    true,
                );
                assert_collision(
                    result,
                    "database",
                    "pool_size",
                    &["pool-size", "pool_size"],
                    &format!("{} / {action_key}", adapter.name()),
                );
            }
        }
    }

    #[test]
    fn normalized_set_rejects_ambiguous_intermediate_table() {
        // The collision check runs at EVERY traversed table, not just the
        // leaf: two spellings of an intermediate section are just as
        // ambiguous. Section is empty for a root-level collision.
        use crate::runtime::{Field, Schema};
        let schema = Schema::object("T")
            .nested(
                "my_db",
                Schema::object("D").field("size", Field::integer().optional()),
            )
            .build();
        let doc = "[my-db]\nsize = 1\n\n[my_db]\nsize = 2\n";
        let result = set_in_document(
            &TomlAdapter,
            &Shape::Object(schema.clone()),
            Some(doc),
            "my_db.size",
            "3",
            true,
        );
        assert_collision(result, "", "my_db", &["my-db", "my_db"], "intermediate");
    }

    #[test]
    fn normalized_set_rejects_collision_off_the_requested_path() {
        // The whole document is validated, not just the traversed path:
        // the requested key (`host`, root level) is unambiguous, but the
        // untraversed `database` table holds both spellings — the edit
        // still fails, because a document the load path refuses is never
        // edited.
        for (adapter, doc) in colliding_docs() {
            let doc = with_host(adapter, doc);
            let result = set_in_document(
                adapter,
                &Shape::Object(test_schema()),
                Some(&doc),
                "host",
                "h2",
                true,
            );
            assert_collision(
                result,
                "database",
                "pool_size",
                &["pool-size", "pool_size"],
                adapter.name(),
            );
        }
    }

    #[test]
    fn normalized_unset_rejects_collision_off_the_requested_path() {
        // Unset runs the same whole-document validation.
        for (adapter, doc) in colliding_docs() {
            let doc = with_host(adapter, doc);
            let result = unset_in_document(adapter, &doc, "host", true);
            assert_collision(
                result,
                "database",
                "pool_size",
                &["pool-size", "pool_size"],
                adapter.name(),
            );
        }
    }

    /// The colliding fixture with an unambiguous root-level `host` key
    /// added, so a test can target a key that never touches the
    /// colliding table.
    fn with_host(adapter: &dyn FormatAdapter, doc: &str) -> String {
        match adapter.name() {
            "toml" => format!("host = \"h\"\n{doc}"),
            "yaml" => format!("host: h\n{doc}"),
            "json" => doc.replacen("{\n", "{\n  \"host\": \"h\",\n", 1),
            other => panic!("unexpected adapter {other}"),
        }
    }

    #[test]
    fn normalized_unset_rejects_ambiguous_spellings() {
        // Unset resolves through the same collision-aware traversal.
        for (adapter, doc) in colliding_docs() {
            for action_key in ["database.pool-size", "database.pool_size"] {
                let result = unset_in_document(adapter, doc, action_key, true);
                assert_collision(
                    result,
                    "database",
                    "pool_size",
                    &["pool-size", "pool_size"],
                    &format!("{} / {action_key}", adapter.name()),
                );
            }
        }
    }

    #[test]
    fn persist_collision_error_carries_file_path() {
        // The document-level functions have no file path; the I/O
        // wrappers stamp the edited file onto the collision so the error
        // matches the load path's shape.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[database]\npool-size = 5\npool_size = 6\n").unwrap();

        for result in [
            persist_value(
                &TomlAdapter,
                &Shape::Object(test_schema()),
                &path,
                "database.pool_size",
                "20",
                true,
            ),
            unset_value(&TomlAdapter, &path, "database.pool_size", true),
        ] {
            match result.unwrap_err() {
                ClapfigError::NormalizedKeyCollision { path: reported, .. } => {
                    assert_eq!(reported, path);
                }
                other => panic!("expected NormalizedKeyCollision, got {other:?}"),
            }
        }
        // The ambiguous file was not modified.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("pool-size = 5") && content.contains("pool_size = 6"));
    }

    #[test]
    fn normalized_unset_missing_key_is_noop() {
        let out =
            unset_in_document(&TomlAdapter, "host = \"h\"\n", "database.pool_size", true).unwrap();
        assert!(out.contains("host = \"h\""));
    }

    #[test]
    fn unset_without_normalization_requires_exact_spelling() {
        // With normalization off there is no equivalence: a kebab entry
        // is not touched by the snake key (and would be an unknown key at
        // load time anyway).
        let out = unset_doc("[database]\npool-size = 5\n", "database.pool_size").unwrap();
        assert!(out.contains("pool-size = 5"));
    }

    // --- unset tests ---

    fn unset_doc(content: &str, key: &str) -> Result<String, ClapfigError> {
        unset_in_document(&TomlAdapter, content, key, false)
    }

    #[test]
    fn unset_removes_key() {
        let content = "port = 8080\nhost = \"localhost\"\n";
        let result = unset_doc(content, "port").unwrap();
        assert!(!result.contains("port"));
        assert!(result.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_nested_key() {
        let content = "[database]\npool_size = 5\nurl = \"pg://\"\n";
        let result = unset_doc(content, "database.pool_size").unwrap();
        assert!(!result.contains("pool_size"));
        assert!(result.contains("url = \"pg://\""));
    }

    #[test]
    fn unset_nonexistent_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_doc(content, "missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_nonexistent_nested_key_is_noop() {
        let content = "port = 8080\n";
        let result = unset_doc(content, "database.missing").unwrap();
        assert!(result.contains("port = 8080"));
    }

    #[test]
    fn unset_preserves_comments_on_other_keys() {
        let content = "port = 8080\n# The host address\nhost = \"localhost\"\n";
        let result = unset_doc(content, "port").unwrap();
        assert!(result.contains("# The host address"));
        assert!(result.contains("host = \"localhost\""));
        assert!(!result.contains("port"));
    }

    #[test]
    fn unset_value_removes_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "port = 8080\nhost = \"localhost\"\n").unwrap();

        let result = unset_value(&TomlAdapter, &path, "port", false).unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));

        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"localhost\""));
    }

    #[test]
    fn unset_value_missing_file_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let result = unset_value(&TomlAdapter, &path, "port", false).unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
    }

    fn tagged_block() -> Shape {
        use crate::runtime::{Field, Schema};
        Shape::from(
            Shape::tagged("Block", "kind")
                .variant(
                    "rust",
                    Schema::object("Rust")
                        .field("mount", Field::string())
                        .field("crate_path", Field::string().optional())
                        .build(),
                )
                .variant(
                    "payload",
                    Schema::object("Payload")
                        .field("mount", Field::string())
                        .field("artifact", Field::string())
                        .build(),
                )
                .build(),
        )
    }

    fn nested_tagged_app() -> Shape {
        use crate::runtime::Schema;
        Shape::from(Schema::object("App").field("block", tagged_block()).build())
    }

    #[test]
    fn set_tagged_tag_rejects_numeric_looking_invalid_discriminator() {
        let err = set_in_document(
            &TomlAdapter,
            &tagged_block(),
            Some("kind = \"rust\"\nmount = \".\"\n"),
            "kind",
            "123",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "kind");
                assert!(reason.contains("not in allowed set"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_tagged_string_field_keeps_numeric_looking_value() {
        let result = set_in_document(
            &TomlAdapter,
            &tagged_block(),
            Some("kind = \"rust\"\nmount = \".\"\n"),
            "mount",
            "123",
            false,
        )
        .unwrap();
        assert!(result.contains("mount = \"123\""), "{result}");
    }

    #[test]
    fn set_nested_tagged_tag_rejects_numeric_looking_invalid_discriminator() {
        let err = set_in_document(
            &TomlAdapter,
            &nested_tagged_app(),
            Some("[block]\nkind = \"rust\"\nmount = \".\"\n"),
            "block.kind",
            "123",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::InvalidValue { key, reason, .. } => {
                assert_eq!(key, "block.kind");
                assert!(reason.contains("not in allowed set"), "{reason}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn set_variant_exclusive_field_without_discriminator_refuses() {
        let err = set_in_document(
            &TomlAdapter,
            &tagged_block(),
            Some(""),
            "artifact",
            "out",
            false,
        )
        .unwrap_err();
        match err {
            ClapfigError::UnaddressableKey { key, kind, .. } => {
                assert_eq!(key, "artifact");
                assert_eq!(kind, "a tagged union");
            }
            other => panic!("expected UnaddressableKey, got {other:?}"),
        }
    }

    #[test]
    fn set_variant_exclusive_field_uses_selected_discriminator() {
        let result = set_in_document(
            &TomlAdapter,
            &tagged_block(),
            Some("kind = \"payload\"\nmount = \".\"\nartifact = \"old\"\n"),
            "artifact",
            "123",
            false,
        )
        .unwrap();
        assert!(result.contains("artifact = \"123\""), "{result}");
    }

    fn kebab_tagged_block() -> Shape {
        use crate::runtime::{Field, Schema};
        Shape::from(
            Shape::tagged("Block", "block_kind")
                .variant(
                    "rust",
                    Schema::object("Rust")
                        .field("mount", Field::string())
                        .field("crate_path", Field::string().optional())
                        .build(),
                )
                .variant(
                    "payload",
                    Schema::object("Payload")
                        .field("mount", Field::string())
                        .field("artifact", Field::string())
                        .build(),
                )
                .build(),
        )
    }

    fn nested_kebab_tagged_app() -> Shape {
        use crate::runtime::Schema;
        Shape::from(
            Schema::object("App")
                .field("site_block", kebab_tagged_block())
                .build(),
        )
    }

    #[test]
    fn set_variant_exclusive_field_in_kebab_tagged_document() {
        let result = set_in_document(
            &TomlAdapter,
            &kebab_tagged_block(),
            Some("block-kind = \"payload\"\nmount = \".\"\nartifact = \"old\"\n"),
            "artifact",
            "123",
            true,
        )
        .unwrap();
        assert!(result.contains("artifact = \"123\""), "{result}");
        assert!(result.contains("block-kind = \"payload\""), "{result}");
    }

    #[test]
    fn set_nested_variant_exclusive_field_in_kebab_tagged_document() {
        let result = set_in_document(
            &TomlAdapter,
            &nested_kebab_tagged_app(),
            Some("[site-block]\nblock-kind = \"payload\"\nmount = \".\"\nartifact = \"old\"\n"),
            "site-block.artifact",
            "123",
            true,
        )
        .unwrap();
        assert!(result.contains("artifact = \"123\""), "{result}");
        assert!(result.contains("block-kind = \"payload\""), "{result}");
    }

    #[test]
    fn colliding_discriminator_spellings_fail_as_normalized_key_collision() {
        // Both spellings, different variants: collision must win over
        // picking one discriminator and then UnaddressableKey / a typed
        // write against the wrong branch.
        let result = set_in_document(
            &TomlAdapter,
            &kebab_tagged_block(),
            Some("block-kind = \"payload\"\nblock_kind = \"rust\"\nmount = \".\"\n"),
            "artifact",
            "123",
            true,
        );
        assert_collision(
            result,
            "",
            "block_kind",
            &["block-kind", "block_kind"],
            "root tagged discriminator collision",
        );
    }

    #[test]
    fn colliding_nested_discriminator_spellings_fail_as_normalized_key_collision() {
        let result = set_in_document(
            &TomlAdapter,
            &nested_kebab_tagged_app(),
            Some("[site-block]\nblock-kind = \"payload\"\nblock_kind = \"rust\"\nmount = \".\"\n"),
            "site-block.artifact",
            "123",
            true,
        );
        assert_collision(
            result,
            "site_block",
            "block_kind",
            &["block-kind", "block_kind"],
            "nested tagged discriminator collision",
        );
    }
}
