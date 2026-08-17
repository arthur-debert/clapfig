//! Entry points for building a clapfig configuration.
//!
//! [`Clapfig`] is the front door. Two entry points cover the two schema
//! sources:
//!
//! - [`Clapfig::schema_builder::<C>()`](Clapfig::schema_builder) — the typed
//!   path: `C` derives [`Schema`](crate::Schema) via
//!   `#[derive(clapfig::Schema)]`, and `load()` returns a typed `C`.
//! - [`Clapfig::runtime(schema)`](Clapfig::runtime) — the runtime path: the
//!   schema is an owned [`runtime::Schema`](crate::runtime::Schema) built at
//!   run time, and `load()` returns a value [`Map`](crate::value::Map).
//!
//! Both produce builders with the same surface — `app_name`, `search_paths`,
//! `env_prefix`, `cli_override`, `post_validate`, `load`, `handle` — backed
//! by one shared resolve pipeline.

use crate::error::ClapfigError;

/// Entry point for building a clapfig configuration.
pub struct Clapfig;

impl Clapfig {
    /// Entry point for the runtime path: build a config pipeline from an
    /// owned [`Schema`](crate::runtime::Schema) instead of a compile-time
    /// derive.
    ///
    /// Returns a [`RuntimeBuilder`](crate::RuntimeBuilder) with the same
    /// surface as [`Self::schema_builder`] — `app_name`, `search_paths`,
    /// `env_prefix`, `cli_override`, `post_validate`, `load`, `handle`,
    /// `build_resolver`, etc. — but produces a value
    /// [`Map`](crate::value::Map) rather than a typed `C`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use clapfig::{Clapfig, runtime::{Field, Schema}};
    ///
    /// let schema = Schema::object("App")
    ///     .field("port", Field::integer().default(8080i64))
    ///     .build();
    ///
    /// let table: clapfig::value::Map = Clapfig::runtime(schema)
    ///     .app_name("myapp")
    ///     .load()?;
    /// ```
    pub fn runtime(schema: crate::runtime::Schema) -> crate::runtime_builder::RuntimeBuilder {
        crate::runtime_builder::RuntimeBuilder::new(schema)
    }

    /// Entry point for the typed path: build a config pipeline from a
    /// struct whose schema was emitted by the `#[derive(clapfig::Schema)]`
    /// macro.
    ///
    /// Same builder surface as [`Self::runtime`], but `load()` returns the
    /// typed `C`. The schema metadata available to JSON Schema, template,
    /// and persistence emission is the full runtime view — full type info,
    /// enum sets, and doc comments, identical to the runtime path.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use clapfig::{Clapfig, Schema};
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Schema, Serialize, Deserialize, Debug)]
    /// struct AppConfig {
    ///     /// App host.
    ///     #[clapfig(default = "localhost")]
    ///     host: String,
    ///
    ///     /// App port.
    ///     #[clapfig(default = 8080)]
    ///     port: u16,
    /// }
    ///
    /// let cfg: AppConfig = Clapfig::schema_builder::<AppConfig>()
    ///     .app_name("myapp")
    ///     .load()?;
    /// ```
    pub fn schema_builder<C: crate::static_schema::Schema>()
    -> crate::schema_builder::SchemaConfigBuilder<C> {
        crate::schema_builder::SchemaConfigBuilder::new()
    }
}

/// Validate a list of `(path, strict)` overrides against a schema and
/// collect them into a [`StrictnessOverrides`](crate::strict::StrictnessOverrides).
/// Shared by the runtime builder (and, through it, the typed
/// `SchemaConfigBuilder`) so every path applies identical typo-protection
/// rules to `strict_at` arguments.
///
/// Errors as [`ClapfigError::InvalidStrictPath`] when:
///
/// - the path does not resolve to any field in the schema, or
/// - the path resolves to a leaf field (strict is a container property).
///
/// When `normalize_keys` is `true`, each path is rewritten through
/// `normalize::normalize_key` before lookup so the override accepts the
/// same kebab/snake spellings the rest of the pipeline does.
pub(crate) fn build_strict_overrides(
    entries: &[(String, bool)],
    normalize_keys: bool,
    schema: crate::spec::SchemaRef<'_>,
) -> Result<crate::strict::StrictnessOverrides, ClapfigError> {
    use crate::strict::{PathKind, StrictnessOverrides, resolve_path_kind};

    let mut out = StrictnessOverrides::from_schema(schema);
    for (raw_path, strict) in entries {
        let path = if normalize_keys {
            crate::normalize::normalize_key(raw_path)
        } else {
            raw_path.clone()
        };
        match resolve_path_kind(schema, &path) {
            PathKind::Section => out.insert(path, *strict),
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
