//! Schema abstraction: a single interchange shape for every internal consumer.
//!
//! [`SchemaRef::from_dynamic`] borrows an owned [`crate::runtime::Schema`] —
//! built either via the runtime builder (`Clapfig::runtime(schema)`) or by
//! the `#[derive(clapfig::Schema)]` macro's cached conversion
//! (`Clapfig::schema_builder::<C>()`).
//!
//! Every consumer that walks `SchemaRef` — strict-mode validation, doc
//! lookup, valid-key enumeration, JSON Schema generation, template
//! generation, persistence validation — works over the same borrowed view,
//! so both entry points share one implementation.
//!
//! Nothing in this module is part of the public crate API.

use std::path::Path;

use crate::error::ClapfigError;
use crate::runtime::{self, LeafType, Schema};
use crate::value::{Map, Value};

/// Borrowed, read-only view of a config schema node.
///
/// Constructed via [`SchemaRef::from_dynamic`].
#[derive(Clone, Copy)]
pub(crate) struct SchemaRef<'a> {
    schema: &'a Schema,
}

impl<'a> SchemaRef<'a> {
    pub fn from_dynamic(schema: &'a Schema) -> Self {
        SchemaRef { schema }
    }

    pub fn name(&self) -> &'a str {
        self.schema.name.as_str()
    }

    pub fn doc(&self) -> &'a [String] {
        &self.schema.doc
    }

    /// Explicit `strict` setting on this node.
    ///
    /// Populated by [`runtime::Schema::strict`]. Consumed by
    /// [`crate::strict::StrictnessOverrides::from_schema`] during the
    /// cascade build.
    pub fn strict(&self) -> Option<bool> {
        self.schema.strict
    }

    /// Iterate the fields declared at this schema level.
    pub fn fields(&self) -> SchemaFieldsIter<'a> {
        SchemaFieldsIter {
            fields: &self.schema.fields,
            index: 0,
        }
    }
}

/// Iterator over a [`SchemaRef`]'s fields. Nameable (not `impl Iterator`) so
/// it threads through generic consumers without a hidden lifetime.
pub(crate) struct SchemaFieldsIter<'a> {
    fields: &'a [runtime::NamedField],
    index: usize,
}

impl<'a> Iterator for SchemaFieldsIter<'a> {
    type Item = FieldRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.fields.len() {
            return None;
        }
        let f = &self.fields[self.index];
        self.index += 1;
        Some(FieldRef::from_dynamic(f))
    }
}

/// Borrowed view of a single named field. `Copy` so it threads through
/// consumers without cloning.
#[derive(Clone, Copy)]
pub(crate) struct FieldRef<'a> {
    pub name: &'a str,
    pub doc: &'a [String],
    pub kind: FieldKindRef<'a>,
}

impl<'a> FieldRef<'a> {
    fn from_dynamic(f: &'a runtime::NamedField) -> Self {
        let (doc, kind) = match &f.field {
            runtime::Field::Leaf(leaf) => {
                let leaf_ref = LeafRef {
                    default: leaf.default.as_ref(),
                    env: leaf.env.as_deref(),
                    optional: leaf.optional,
                    ty: &leaf.ty,
                };
                (leaf.doc.as_slice(), FieldKindRef::Leaf(leaf_ref))
            }
            runtime::Field::Nested(schema) => (
                schema.doc.as_slice(),
                FieldKindRef::Nested {
                    schema: SchemaRef::from_dynamic(schema),
                },
            ),
            runtime::Field::ArrayOf(schema) => (
                schema.doc.as_slice(),
                FieldKindRef::ArrayOf {
                    schema: SchemaRef::from_dynamic(schema),
                },
            ),
            runtime::Field::MapOf(schema) => (
                schema.doc.as_slice(),
                FieldKindRef::MapOf {
                    schema: SchemaRef::from_dynamic(schema),
                },
            ),
        };
        FieldRef {
            name: f.name.as_str(),
            doc,
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
    /// Array-of-objects, TOML `[[name]]`. Produced by `Field::ArrayOf(...)`.
    ArrayOf {
        schema: SchemaRef<'a>,
    },
    /// Map-of-objects, TOML `[name.<key>]` with arbitrary user-supplied
    /// keys. Each entry's value is itself an object validated against
    /// `schema`. Produced by `Field::MapOf(...)` and by the derive macro
    /// for `HashMap<String, NestedStruct>` / `BTreeMap<String, NestedStruct>`
    /// fields.
    MapOf {
        schema: SchemaRef<'a>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct LeafRef<'a> {
    /// Default value for the leaf, when declared.
    pub default: Option<&'a Value>,
    pub env: Option<&'a str>,
    pub optional: bool,
    /// Declared leaf type. Always present: every schema leaf carries an
    /// explicit [`LeafType`], so JSON Schema emission and value validation
    /// read the declared shape directly. Enum leaves carry their allowed
    /// values inside [`LeafType::Enum`].
    pub ty: &'a LeafType,
}

impl<'a> LeafRef<'a> {
    /// Allowed-value constraint for the leaf: the value set of a
    /// [`LeafType::Enum`] leaf, `None` for every other leaf type.
    pub fn allowed_values(&self) -> Option<&'a [Value]> {
        match self.ty {
            LeafType::Enum { values } => Some(values.as_slice()),
            _ => None,
        }
    }
}

/// Strategy interface decoupling the resolve pipeline from the schema
/// source.
///
/// Implemented by `DynamicSpec` (see [`crate::runtime_spec`]), which walks
/// an owned [`Schema`]. The resolve pipeline invokes three hooks in order:
/// `validate_unknown` (strict-mode check on every parsed file),
/// `fill_defaults` (called once on the merged table just before finalize),
/// `finalize` (produce the typed `Output`). The fourth hook, `schema`, is
/// for consumers that need to walk the structure independently.
pub(crate) trait ConfigSpec {
    /// The final, typed output produced by [`finalize`](Self::finalize).
    /// `DynamicSpec` returns a value [`Map`].
    type Output;

    /// The schema as a borrowed view. Consumed by the env-layer
    /// validator in `resolve::resolve` and by spec-aware consumers
    /// (handle, persist).
    fn schema(&self) -> SchemaRef<'_>;

    /// Detect unknown keys in a parsed config table.
    ///
    /// `ctx` carries the strictness cascade overrides, the builder-level
    /// default, the optional `on_unknown_key` callback, and the
    /// `normalize_keys` flag for the line-number heuristic.
    ///
    /// Returns the keys the callback opted to
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect)
    /// — empty for callers that don't use the collect path. Reject
    /// decisions surface as `ClapfigError::UnknownKeys`.
    fn validate_unknown(
        &self,
        table: &Map,
        source: &str,
        path: &Path,
        ctx: &crate::validate::ValidateContext<'_>,
    ) -> Result<Vec<crate::strict::CollectedUnknown>, ClapfigError>;

    /// Inject default values into a merged table before finalization.
    ///
    /// Walks the schema and populates missing leaves from their `default`
    /// values directly into the table.
    fn fill_defaults(&self, table: &mut Map) -> Result<(), ClapfigError>;

    /// Finalize a merged table into the spec's `Output`.
    fn finalize(&self, merged: Map) -> Result<Self::Output, ClapfigError>;
}
