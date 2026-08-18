//! Builder for configs whose schema comes from `#[derive(clapfig::Schema)]`.
//!
//! Entry point: [`crate::Clapfig::typed::<C>()`](crate::Clapfig::typed).
//!
//! Internally this wraps a [`Builder`](crate::Builder)
//! constructed from `C::schema()` (the cached [`runtime::Schema`] view
//! of the macro-emitted `SchemaStatic`). Every method forwards through
//! to the Map-out builder so both paths share one resolve pipeline. The
//! only added work is the final `Map → C` deserialize step on `load()`
//! (through the value model's serde bridge) and the typed
//! `post_validate(&C)` hook.
//!
//! [`runtime::Schema`]: crate::runtime::Schema

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::builder::Builder;
use crate::error::ClapfigError;
use crate::ops::ConfigResult;
use crate::static_schema::Schema;
use crate::types::{ConfigAction, Layer, SearchMode, SearchPath};
use crate::value::{Map, Value, from_value};

/// Typed-config builder driven by a `#[derive(clapfig::Schema)]` struct.
///
/// Same surface as [`Builder`](crate::Builder) — `app_name`,
/// `search_paths`, `env_prefix`, `cli_override`, `post_validate`, `load`,
/// `handle` — but `load()` returns the typed `C` and `post_validate`
/// receives a typed `&C`.
pub struct TypedBuilder<C: Schema> {
    inner: Builder,
    _phantom: PhantomData<fn() -> C>,
}

impl<C: Schema> TypedBuilder<C> {
    pub(crate) fn new() -> Self {
        // Reuse the per-type `Arc<Schema>` cache the derive maintains —
        // one `Arc::clone` (atomic increment, no allocation) per builder
        // construction instead of a full schema-tree clone.
        Self {
            inner: Builder::from_arc(C::schema_arc()),
            _phantom: PhantomData,
        }
    }

    /// Set the application name. See
    /// [`Builder::app_name`](crate::Builder::app_name).
    pub fn app_name(mut self, name: &str) -> Self {
        self.inner = self.inner.app_name(name);
        self
    }

    /// Override the config file name. See
    /// [`Builder::file_name`](crate::Builder::file_name).
    pub fn file_name(mut self, name: &str) -> Self {
        self.inner = self.inner.file_name(name);
        self
    }

    /// Discover config files by stem across the enabled formats. See
    /// [`Builder::file_stem`](crate::Builder::file_stem).
    pub fn file_stem(mut self, stem: &str) -> Self {
        self.inner = self.inner.file_stem(stem);
        self
    }

    /// Set the ordered enabled-formats list. See
    /// [`Builder::formats`](crate::Builder::formats).
    pub fn formats<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner = self.inner.formats(names);
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

    /// Convenience: "accept dotted, reject bare" at a dotted-path
    /// subtree. See
    /// [`Builder::accept_dotted_extension_keys_in`](crate::Builder::accept_dotted_extension_keys_in)
    /// for the full semantics.
    pub fn accept_dotted_extension_keys_in(
        mut self,
        path: &str,
        decision: crate::UnknownKeyDecision,
    ) -> Self {
        self.inner = self.inner.accept_dotted_extension_keys_in(path, decision);
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

impl<C: Schema + DeserializeOwned> TypedBuilder<C> {
    /// Post-merge validation hook. Receives the typed `&C`.
    ///
    /// Conceptually the same as
    /// [`Builder::post_validate`](crate::Builder::post_validate)
    /// — internally we deserialize the merged value [`Map`] into `C`
    /// inside the Map-out builder's hook so the user's closure stays
    /// typed.
    pub fn post_validate<F>(mut self, f: F) -> Self
    where
        F: Fn(&C) -> Result<(), String> + Send + Sync + 'static,
    {
        self.inner = self.inner.post_validate(move |t: &Map| {
            let typed: C = from_value(Value::Map(t.clone())).map_err(|e| e.to_string())?;
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
    /// The action surface is identical to the Map-out path —
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

fn deserialize_table<C: DeserializeOwned>(table: Map) -> Result<C, ClapfigError> {
    // The value model's serde bridge carries datetimes through its
    // private marker struct, so this deserializes directly — no
    // serialize-reparse round trip (the hack the owned model retired).
    from_value(Value::Map(table)).map_err(|e| ClapfigError::InvalidValue {
        key: "<merged>".into(),
        reason: e.to_string(),
    })
}
