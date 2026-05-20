//! Builder for configs whose schema comes from `#[derive(clapfig::Schema)]`.
//!
//! Entry point: [`crate::Clapfig::schema_builder::<C>()`](crate::Clapfig::schema_builder).
//!
//! Internally this wraps a [`RuntimeBuilder`](crate::RuntimeBuilder)
//! constructed from `C::schema()` (the cached runtime view of the
//! macro-emitted `SchemaStatic`). Every method forwards through to the
//! runtime builder so the static and runtime paths share one resolve
//! pipeline. The only added work is the final `Table → C` deserialize
//! step on `load()` and the typed `post_validate(&C)` hook.
//!
//! ## Why a separate builder
//!
//! `ClapfigBuilder<C: confique::Config>` (the existing static path) and
//! `SchemaConfigBuilder<C: clapfig::Schema>` (this one) share most of
//! their surface but bind on different schema-source traits. Keeping
//! them as separate types lets the confique-driven path keep working
//! byte-identically while the new macro-driven path moves to the runtime
//! pipeline.

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;
use toml::{Table, Value};

use crate::error::ClapfigError;
use crate::ops::ConfigResult;
use crate::runtime_builder::RuntimeBuilder;
use crate::static_schema::Schema;
use crate::types::{ConfigAction, Layer, SearchMode, SearchPath};

/// Typed-config builder driven by a `#[derive(clapfig::Schema)]` struct.
///
/// Parallel to [`ClapfigBuilder<C>`](crate::ClapfigBuilder) for the
/// confique-derived path. Same surface — `app_name`, `search_paths`,
/// `env_prefix`, `cli_override`, `post_validate`, `load`, `handle` — but
/// the schema comes from the new derive macro rather than confique's
/// `Meta` tree.
pub struct SchemaConfigBuilder<C: Schema> {
    inner: RuntimeBuilder,
    _phantom: PhantomData<fn() -> C>,
}

impl<C: Schema> SchemaConfigBuilder<C> {
    pub(crate) fn new() -> Self {
        // Reuse the per-type `Arc<Schema>` cache the derive maintains —
        // one `Arc::clone` (atomic increment, no allocation) per builder
        // construction instead of a full schema-tree clone.
        Self {
            inner: RuntimeBuilder::from_arc(C::schema_arc()),
            _phantom: PhantomData,
        }
    }

    /// Set the application name. See
    /// [`ClapfigBuilder::app_name`](crate::ClapfigBuilder::app_name).
    pub fn app_name(mut self, name: &str) -> Self {
        self.inner = self.inner.app_name(name);
        self
    }

    /// Override the config file name. See
    /// [`ClapfigBuilder::file_name`](crate::ClapfigBuilder::file_name).
    pub fn file_name(mut self, name: &str) -> Self {
        self.inner = self.inner.file_name(name);
        self
    }

    /// Replace the default search paths entirely.
    pub fn search_paths(mut self, paths: Vec<SearchPath>) -> Self {
        self.inner = self.inner.search_paths(paths);
        self
    }

    /// Append a single search path.
    pub fn add_search_path(mut self, path: SearchPath) -> Self {
        self.inner = self.inner.add_search_path(path);
        self
    }

    /// Set the search mode.
    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.inner = self.inner.search_mode(mode);
        self
    }

    /// Register a named persist scope for `config set`/`unset`.
    pub fn persist_scope(mut self, name: &str, path: SearchPath) -> Self {
        self.inner = self.inner.persist_scope(name, path);
        self
    }

    /// Override the env var prefix.
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.inner = self.inner.env_prefix(prefix);
        self
    }

    /// Disable env loading entirely.
    pub fn no_env(mut self) -> Self {
        self.inner = self.inner.no_env();
        self
    }

    /// Set the whole-resolution strictness default.
    pub fn strict(mut self, strict: bool) -> Self {
        self.inner = self.inner.strict(strict);
        self
    }

    /// Set per-section strictness for a dotted path.
    pub fn strict_at(mut self, path: &str, strict: bool) -> Self {
        self.inner = self.inner.strict_at(path, strict);
        self
    }

    /// Register a per-key callback for cascade-rejected unknown keys.
    pub fn on_unknown_key<F>(mut self, callback: F) -> Self
    where
        F: Fn(&crate::UnknownKeyContext<'_>) -> crate::UnknownKeyDecision + Send + Sync + 'static,
    {
        self.inner = self.inner.on_unknown_key(callback);
        self
    }

    /// Accept kebab-case keys in config files and CLI/URL overrides.
    pub fn normalize_keys(mut self, normalize: bool) -> Self {
        self.inner = self.inner.normalize_keys(normalize);
        self
    }

    /// Set a custom layer merge order.
    pub fn layer_order(mut self, order: Vec<Layer>) -> Self {
        self.inner = self.inner.layer_order(order);
        self
    }

    /// Add a URL query string as a config layer.
    #[cfg(feature = "url")]
    pub fn url_query(mut self, query: &str) -> Self {
        self.inner = self.inner.url_query(query);
        self
    }

    /// Add a single CLI override.
    pub fn cli_override<V: Into<Value>>(mut self, key: &str, value: Option<V>) -> Self {
        self.inner = self.inner.cli_override(key, value);
        self
    }

    /// Match a serializable struct's fields against the schema's keys.
    pub fn cli_overrides_from<S: Serialize>(mut self, source: &S) -> Self {
        self.inner = self.inner.cli_overrides_from(source);
        self
    }
}

impl<C: Schema + DeserializeOwned> SchemaConfigBuilder<C> {
    /// Post-merge validation hook. Receives the typed `&C`.
    ///
    /// Conceptually the same as
    /// [`ClapfigBuilder::post_validate`](crate::ClapfigBuilder::post_validate)
    /// — internally we deserialize the merged `toml::Table` into `C`
    /// inside the runtime builder's hook so the user's closure stays
    /// typed.
    pub fn post_validate<F>(mut self, f: F) -> Self
    where
        F: Fn(&C) -> Result<(), String> + Send + Sync + 'static,
    {
        self.inner = self.inner.post_validate(move |t: &Table| {
            // Match `load()`'s datetime-safe round-trip — see
            // `deserialize_table` for why try_into isn't enough.
            let text = toml::to_string(&Value::Table(t.clone()))
                .map_err(|e: toml::ser::Error| e.to_string())?;
            let typed: C = toml::from_str(&text).map_err(|e: toml::de::Error| e.to_string())?;
            f(&typed)
        });
        self
    }

    /// Load and resolve the configuration through all layers, returning a
    /// typed `C`.
    pub fn load(self) -> Result<C, ClapfigError> {
        let table = self.inner.load()?;
        deserialize_table::<C>(table)
    }

    /// Same as [`load`](Self::load) but also returns any keys the
    /// [`on_unknown_key`](Self::on_unknown_key) callback elected to
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    /// The list is empty when no callback is registered or no key opts in.
    pub fn load_with_unknowns(
        self,
    ) -> Result<(C, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        let (table, unknowns) = self.inner.load_with_unknowns()?;
        let typed = deserialize_table::<C>(table)?;
        Ok((typed, unknowns))
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output.
    ///
    /// The action surface is identical to the runtime path —
    /// `gen | schema | get | list | set | unset` all delegate.
    pub fn handle(self, action: &ConfigAction) -> Result<ConfigResult, ClapfigError> {
        self.inner.handle(action)
    }

    /// Dispatch a [`ConfigAction`] and print the result.
    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError> {
        self.inner.handle_and_print(action)
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output as a
    /// `String`.
    pub fn handle_to_string(self, action: &ConfigAction) -> Result<String, ClapfigError> {
        self.inner.handle_to_string(action)
    }
}

fn deserialize_table<C: DeserializeOwned>(table: Table) -> Result<C, ClapfigError> {
    // `toml::Value::try_into` goes through a value-tree deserializer that
    // doesn't preserve `toml::value::Datetime`'s special-struct marker —
    // a `Value::Datetime` in the table arrives at the field's
    // `Deserialize` impl as a plain string and the deserialize fails with
    // "expected a TOML datetime". Serializing back to text and re-parsing
    // routes through the TOML lexer, which keeps datetimes typed all the
    // way to the field. One extra alloc on load is fine; correctness
    // wins.
    let text = toml::to_string(&Value::Table(table)).map_err(|e: toml::ser::Error| {
        ClapfigError::InvalidValue {
            key: "<merged>".into(),
            reason: e.to_string(),
        }
    })?;
    toml::from_str(&text).map_err(|e: toml::de::Error| ClapfigError::InvalidValue {
        key: "<merged>".into(),
        reason: e.to_string(),
    })
}
