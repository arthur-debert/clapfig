//! Schema abstraction: a single interchange shape for every internal consumer
//! that today keys off `confique::meta::Meta`.
//!
//! Two adapter paths plug into the same shape:
//!
//! - [`SchemaRef::from_meta`] borrows a `confique::meta::Meta` (static path,
//!   `ClapfigBuilder<C: Config>`).
//! - [`SchemaRef::from_dynamic`] borrows an owned [`crate::runtime::Schema`]
//!   built via the runtime builder (`Clapfig::runtime(schema)`).
//!
//! Every consumer that walks `SchemaRef` — strict-mode validation, doc
//! lookup, valid-key enumeration, JSON Schema generation, template
//! generation, persistence validation — works over either source without
//! a recompile-time struct.
//!
//! Nothing in this module is part of the public crate API.

use std::marker::PhantomData;
use std::path::Path;

use confique::Config;
use confique::meta::{self, Expr};
use serde::Deserialize;
use toml::Table;

use crate::error::ClapfigError;
use crate::runtime::{self, LeafType, Schema};

/// Borrowed, read-only view of a config schema node.
///
/// Constructed via [`SchemaRef::from_meta`] for the static path or
/// [`SchemaRef::from_dynamic`] for the runtime path.
#[derive(Clone, Copy)]
pub(crate) enum SchemaRef<'a> {
    Static { meta: &'a meta::Meta },
    Dynamic { schema: &'a Schema },
}

impl<'a> SchemaRef<'a> {
    pub fn from_meta(meta: &'a meta::Meta) -> Self {
        SchemaRef::Static { meta }
    }

    pub fn from_dynamic(schema: &'a Schema) -> Self {
        SchemaRef::Dynamic { schema }
    }

    pub fn name(&self) -> &'a str {
        match self {
            SchemaRef::Static { meta } => meta.name,
            SchemaRef::Dynamic { schema } => schema.name.as_str(),
        }
    }

    pub fn doc(&self) -> DocSource<'a> {
        match self {
            SchemaRef::Static { meta } => DocSource::Static(meta.doc),
            SchemaRef::Dynamic { schema } => DocSource::Dynamic(&schema.doc),
        }
    }

    /// Explicit `strict` setting on this node.
    ///
    /// Populated by [`runtime::Schema::strict`] for dynamic schemas; the
    /// static path always returns `None`. Phase 3 (cascading strictness)
    /// will consume this during unknown-key resolution.
    #[allow(dead_code)] // consumed in Phase 3
    pub fn strict(&self) -> Option<bool> {
        match self {
            SchemaRef::Static { .. } => None,
            SchemaRef::Dynamic { schema } => schema.strict,
        }
    }

    /// Iterate the fields declared at this schema level.
    pub fn fields(&self) -> SchemaFieldsIter<'a> {
        match self {
            SchemaRef::Static { meta } => SchemaFieldsIter::Static {
                fields: meta.fields,
                index: 0,
            },
            SchemaRef::Dynamic { schema } => SchemaFieldsIter::Dynamic {
                fields: &schema.fields,
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
    Dynamic {
        fields: &'a [runtime::NamedField],
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
            SchemaFieldsIter::Dynamic { fields, index } => {
                if *index >= fields.len() {
                    return None;
                }
                let f = &fields[*index];
                *index += 1;
                Some(FieldRef::from_dynamic(f))
            }
        }
    }
}

/// Borrowed view of a single named field. `Copy` so it threads through
/// consumers without cloning.
#[derive(Clone, Copy)]
pub(crate) struct FieldRef<'a> {
    pub name: &'a str,
    pub doc: DocSource<'a>,
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
                    ty: None,
                })
            }
            meta::FieldKind::Nested { meta } => FieldKindRef::Nested {
                schema: SchemaRef::from_meta(meta),
            },
        };
        FieldRef {
            name: f.name,
            doc: DocSource::Static(f.doc),
            kind,
        }
    }

    fn from_dynamic(f: &'a runtime::NamedField) -> Self {
        let (doc_source, kind) = match &f.field {
            runtime::Field::Leaf(leaf) => {
                let allowed_values = match &leaf.ty {
                    LeafType::Enum { values } => Some(values.as_slice()),
                    _ => None,
                };
                let leaf_ref = LeafRef {
                    default: leaf.default.as_ref().map(LeafDefault::Toml),
                    env: leaf.env.as_deref(),
                    optional: leaf.optional,
                    allowed_values,
                    ty: Some(&leaf.ty),
                };
                (DocSource::Dynamic(&leaf.doc), FieldKindRef::Leaf(leaf_ref))
            }
            runtime::Field::Nested(schema) => (
                DocSource::Dynamic(&schema.doc),
                FieldKindRef::Nested {
                    schema: SchemaRef::from_dynamic(schema),
                },
            ),
            runtime::Field::ArrayOf(schema) => (
                DocSource::Dynamic(&schema.doc),
                FieldKindRef::ArrayOf {
                    schema: SchemaRef::from_dynamic(schema),
                },
            ),
        };
        FieldRef {
            name: f.name.as_str(),
            doc: doc_source,
            kind,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FieldKindRef<'a> {
    Leaf(LeafRef<'a>),
    Nested {
        schema: SchemaRef<'a>,
    },
    /// Array-of-objects, TOML `[[name]]`. Static schemas don't produce this
    /// variant; it's exclusive to the runtime path's `Field::ArrayOf(...)`.
    ArrayOf {
        schema: SchemaRef<'a>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct LeafRef<'a> {
    pub default: Option<LeafDefault<'a>>,
    pub env: Option<&'a str>,
    pub optional: bool,
    /// Allowed-value constraint for the leaf. Populated for
    /// runtime `LeafType::Enum { values }` leaves; `None` on the static
    /// path (confique handles its enum variants on the deserialize side).
    pub allowed_values: Option<&'a [toml::Value]>,
    /// Declared leaf type. Populated for the runtime path so JSON Schema
    /// emission and value validation can read the explicit shape; `None`
    /// on the static path (confique's `Meta` only carries default
    /// expressions, not types).
    pub ty: Option<&'a LeafType>,
}

/// Origin-tagged default for a leaf field.
///
/// Static specs carry confique's `Expr` directly so the existing JSON
/// Schema emission keeps working unchanged. Runtime specs carry an owned
/// `toml::Value` reference.
#[derive(Clone, Copy)]
pub(crate) enum LeafDefault<'a> {
    Expr(&'a Expr),
    Toml(&'a toml::Value),
}

/// Origin-tagged source of doc-comment lines.
///
/// Static schemas store `&'static [&'static str]` (one per `///` line);
/// runtime schemas store `Vec<String>` and expose it as a slice. Both
/// flatten to `&str` through [`DocSource::iter`].
#[derive(Clone, Copy)]
pub(crate) enum DocSource<'a> {
    Static(&'a [&'a str]),
    Dynamic(&'a [String]),
}

impl<'a> DocSource<'a> {
    pub fn iter(&self) -> DocLines<'a> {
        match self {
            DocSource::Static(lines) => DocLines::Static(lines.iter()),
            DocSource::Dynamic(lines) => DocLines::Dynamic(lines.iter()),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            DocSource::Static(lines) => lines.is_empty(),
            DocSource::Dynamic(lines) => lines.is_empty(),
        }
    }
}

/// Borrow-collapsing iterator returned by [`DocSource::iter`].
pub(crate) enum DocLines<'a> {
    Static(std::slice::Iter<'a, &'a str>),
    Dynamic(std::slice::Iter<'a, String>),
}

impl<'a> Iterator for DocLines<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        match self {
            DocLines::Static(it) => it.next().copied(),
            DocLines::Dynamic(it) => it.next().map(|s| s.as_str()),
        }
    }
}

/// Strategy interface decoupling the resolve pipeline from `C: Config`.
///
/// The static path (`StaticSpec<C>`) delegates to confique; the runtime
/// path (`DynamicSpec`) walks an owned [`Schema`]. The resolve pipeline
/// invokes three hooks in order: `validate_unknown` (strict-mode check on
/// every parsed file), `fill_defaults` (called once on the merged table
/// just before finalize), `finalize` (produce the typed `Output`). The
/// fourth hook, `schema`, is reserved for consumers that need to walk the
/// structure independently.
pub(crate) trait ConfigSpec {
    /// The final, typed output produced by [`finalize`](Self::finalize).
    /// `StaticSpec<C>` returns `C`; `DynamicSpec` returns `toml::Table`.
    type Output;

    /// The schema as a borrowed view.
    #[allow(dead_code)] // consumed by spec-aware consumers (handle, persist)
    fn schema(&self) -> SchemaRef<'_>;

    /// Detect unknown keys in a parsed config table.
    ///
    /// `ctx` carries the strictness cascade overrides, the builder-level
    /// default, the optional `on_unknown_key` callback, and the
    /// `normalize_keys` flag for the line-number heuristic.
    fn validate_unknown(
        &self,
        table: &Table,
        source: &str,
        path: &Path,
        ctx: &crate::validate::ValidateContext<'_>,
    ) -> Result<(), ClapfigError>;

    /// Inject default values into a merged table before finalization.
    ///
    /// Default impl is a no-op: the static path leaves defaults to confique
    /// (handled inside [`finalize`](Self::finalize)). The runtime path
    /// overrides this to walk the schema and populate missing leaves from
    /// their `default` values directly into the table.
    fn fill_defaults(&self, _table: &mut Table) -> Result<(), ClapfigError> {
        Ok(())
    }

    /// Finalize a merged table into the spec's `Output`.
    fn finalize(&self, merged: Table) -> Result<Self::Output, ClapfigError>;
}

/// Static-path adapter: drives the pipeline from a compile-time `Config` derive.
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
        ctx: &crate::validate::ValidateContext<'_>,
    ) -> Result<(), ClapfigError> {
        crate::validate::validate_unknown_keys::<C>(table, source, path, ctx)
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
