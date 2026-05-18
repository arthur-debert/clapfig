//! Schema abstraction: a single interchange shape for every internal consumer
//! that today keys off `confique::meta::Meta`.
//!
//! The static path (`ClapfigBuilder<C: Config>`) walks `confique::meta::Meta`
//! through a borrowed [`SchemaRef`] view. The planned runtime path (issue #36)
//! will supply a parallel adapter that walks a user-supplied owned `Schema`,
//! so the same consumers — strict-mode validation, doc lookup, valid-key
//! enumeration, JSON Schema generation, template generation, persistence
//! validation — work over either source without a recompile-time struct.
//!
//! Phase 1 only ships the static side. The fields reserved here for later
//! phases (`SchemaRef::strict` for cascading strictness, `LeafRef::allowed_values`
//! for runtime enum constraints) are always `None` on the static path; Phase
//! 3 and Phase 2 respectively will populate them.
//!
//! Nothing in this module is part of the public crate API — `SchemaRef` /
//! `ConfigSpec` / `StaticSpec` are crate-private until Phase 2 introduces
//! `Clapfig::runtime(schema)`.

use std::marker::PhantomData;
use std::path::Path;

use confique::Config;
use confique::meta::{self, Expr};
use serde::Deserialize;
use toml::Table;

use crate::error::ClapfigError;

/// Borrowed, read-only view of a config schema node.
///
/// Constructed via [`SchemaRef::from_meta`] for the static path. Phase 2 will
/// add a `Dynamic` variant borrowing from an owned runtime `Schema`.
#[derive(Clone, Copy)]
pub(crate) enum SchemaRef<'a> {
    Static { meta: &'a meta::Meta },
}

impl<'a> SchemaRef<'a> {
    pub fn from_meta(meta: &'a meta::Meta) -> Self {
        SchemaRef::Static { meta }
    }

    pub fn name(&self) -> &'a str {
        match self {
            SchemaRef::Static { meta } => meta.name,
        }
    }

    pub fn doc(&self) -> &'a [&'a str] {
        match self {
            SchemaRef::Static { meta } => meta.doc,
        }
    }

    /// Explicit `strict` setting on this node.
    ///
    /// Always `None` in Phase 1; Phase 3 (cascading strictness) wires the
    /// per-node override and Phase 2 (`Schema::strict(...)`) populates it on
    /// runtime nodes. Reserved here so the trait surface doesn't widen in
    /// later phases.
    #[allow(dead_code)] // populated and read in Phase 3
    pub fn strict(&self) -> Option<bool> {
        None
    }

    /// Iterate the fields declared at this schema level.
    pub fn fields(&self) -> SchemaFieldsIter<'a> {
        match self {
            SchemaRef::Static { meta } => SchemaFieldsIter::Static {
                fields: meta.fields,
                index: 0,
            },
        }
    }
}

/// Iterator over a [`SchemaRef`]'s fields. Nameable (not `impl Iterator`) so
/// it threads through generic consumers without a hidden lifetime.
pub(crate) enum SchemaFieldsIter<'a> {
    Static {
        fields: &'a [meta::Field],
        index: usize,
    },
}

impl<'a> Iterator for SchemaFieldsIter<'a> {
    type Item = FieldRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SchemaFieldsIter::Static { fields, index } => {
                if *index >= fields.len() {
                    return None;
                }
                let f = &fields[*index];
                *index += 1;
                Some(FieldRef::from_static(f))
            }
        }
    }
}

/// Borrowed view of a single named field.
#[derive(Clone, Copy)]
pub(crate) struct FieldRef<'a> {
    pub name: &'a str,
    pub doc: &'a [&'a str],
    pub kind: FieldKindRef<'a>,
}

impl<'a> FieldRef<'a> {
    fn from_static(f: &'a meta::Field) -> Self {
        let kind = match &f.kind {
            meta::FieldKind::Leaf { env, kind } => {
                let (default, optional) = match kind {
                    meta::LeafKind::Required { default } => (default.as_ref(), false),
                    meta::LeafKind::Optional => (None, true),
                };
                FieldKindRef::Leaf(LeafRef {
                    default: default.map(LeafDefault::Expr),
                    env: *env,
                    optional,
                    allowed_values: None,
                })
            }
            meta::FieldKind::Nested { meta } => FieldKindRef::Nested {
                schema: SchemaRef::from_meta(meta),
            },
        };
        FieldRef {
            name: f.name,
            doc: f.doc,
            kind,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FieldKindRef<'a> {
    Leaf(LeafRef<'a>),
    Nested { schema: SchemaRef<'a> },
}

#[derive(Clone, Copy)]
pub(crate) struct LeafRef<'a> {
    pub default: Option<LeafDefault<'a>>,
    pub env: Option<&'a str>,
    pub optional: bool,
    /// Allowed-value constraint for the leaf.
    ///
    /// Always `None` in Phase 1. Phase 2 (`Field::enum_of(...)`) populates
    /// this for runtime enum leaves; the static path keeps it `None` because
    /// confique handles its enum variants on the deserialize side.
    pub allowed_values: Option<&'a [toml::Value]>,
}

/// Origin-tagged default for a leaf field.
///
/// Static specs carry confique's `Expr` directly so the existing JSON
/// Schema emission keeps working unchanged. Phase 2 will add a `Toml` variant
/// for runtime-supplied defaults.
#[derive(Clone, Copy)]
pub(crate) enum LeafDefault<'a> {
    Expr(&'a Expr),
}

/// Strategy interface decoupling the resolve pipeline from `C: Config`.
///
/// The static path (`StaticSpec<C>`) delegates to confique; the planned
/// runtime path (issue #36) supplies a `DynamicSpec` that walks a user-
/// supplied `Schema`. The pipeline calls four hooks in order: `schema`
/// (for any consumer that needs the structure), `validate_unknown` (strict
/// mode), `fill_defaults` (pre-finalize), `finalize` (produce the typed
/// `Output`).
pub(crate) trait ConfigSpec {
    /// The final, typed output produced by [`finalize`](Self::finalize).
    /// `StaticSpec<C>` returns `C`; the runtime path will return
    /// `toml::Table`.
    type Output;

    /// The schema as a borrowed view.
    ///
    /// Not called by the Phase 1 resolve pipeline (consumers that need the
    /// schema today call helpers like `SchemaRef::from_meta` directly), but
    /// kept on the trait so Phase 2's runtime path can supply its own
    /// schema without a parallel accessor.
    #[allow(dead_code)] // consumed in Phase 2
    fn schema(&self) -> SchemaRef<'_>;

    /// Detect unknown keys in a parsed config table.
    ///
    /// `table` has already been normalized (kebab → snake) if the builder
    /// has `normalize_keys(true)` set; the `normalize_keys` flag is forwarded
    /// only so the line-number heuristic can match keys regardless of dash/
    /// underscore spelling when rendering error snippets.
    fn validate_unknown(
        &self,
        table: &Table,
        source: &str,
        path: &Path,
        normalize_keys: bool,
    ) -> Result<(), ClapfigError>;

    /// Inject default values into a merged table before finalization.
    ///
    /// Default impl is a no-op: the static path leaves defaults to confique
    /// (handled inside [`finalize`](Self::finalize)). The runtime path will
    /// override this to walk the schema and populate missing leaves from
    /// their `default` values directly into the table.
    fn fill_defaults(&self, _table: &mut Table) -> Result<(), ClapfigError> {
        Ok(())
    }

    /// Finalize a merged table into the spec's `Output`.
    ///
    /// Responsible for type-checking values and required-field enforcement.
    /// The static path delegates to confique (`C::Layer` deserialization +
    /// `C::builder().preloaded(...).load()`, which also injects compiled
    /// defaults); the runtime path will check required fields directly and
    /// return the table.
    fn finalize(&self, merged: Table) -> Result<Self::Output, ClapfigError>;
}

/// Static-path adapter: drives the pipeline from a compile-time `Config` derive.
///
/// Zero-sized — the `C` type parameter only feeds confique's static methods.
/// `PhantomData<fn() -> C>` keeps `StaticSpec<C>` unconditionally `Send + Sync`
/// (we never own a `C`).
pub(crate) struct StaticSpec<C> {
    _phantom: PhantomData<fn() -> C>,
}

impl<C> StaticSpec<C> {
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<C> ConfigSpec for StaticSpec<C>
where
    C: Config,
    C::Layer: for<'de> Deserialize<'de>,
{
    type Output = C;

    fn schema(&self) -> SchemaRef<'_> {
        SchemaRef::from_meta(&C::META)
    }

    fn validate_unknown(
        &self,
        table: &Table,
        source: &str,
        path: &Path,
        normalize_keys: bool,
    ) -> Result<(), ClapfigError> {
        crate::validate::validate_unknown_keys::<C>(table, source, path, normalize_keys)
    }

    fn finalize(&self, merged: Table) -> Result<C, ClapfigError> {
        let layer: C::Layer =
            toml::Value::Table(merged)
                .try_into()
                .map_err(|e: toml::de::Error| ClapfigError::InvalidValue {
                    key: "<merged>".into(),
                    reason: e.to_string(),
                })?;
        C::builder()
            .preloaded(layer)
            .load()
            .map_err(ClapfigError::from)
    }
}
