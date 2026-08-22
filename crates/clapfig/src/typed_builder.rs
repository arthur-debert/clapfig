//! Builder for configs whose schema comes from `#[derive(clapfig::Schema)]`.
//!
//! Entry point: [`crate::Clapfig::typed::<C>()`](crate::Clapfig::typed).
//!
//! Internally this wraps a [`Builder`](crate::Builder)
//! constructed from `C::shape_arc()` (the cached document-root
//! [`runtime::Shape`](crate::runtime::Shape), shared clone-free). Every
//! method forwards through to the Map-out builder so both paths share
//! one resolve pipeline. The only added work is the final `Map → C`
//! deserialize step — exactly one per `load()` and per
//! [`TypedResolver::resolve_at`] call (through the value model's serde
//! bridge) — and the typed `post_validate(&C)` hook, which runs on that
//! same deserialized instance.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::builder::{Builder, Resolver};
use crate::error::ClapfigError;
use crate::ops::ConfigResult;
use crate::static_schema::DocumentRoot;
use crate::types::{ConfigAction, Layer, SearchMode, SearchPath};
use crate::value::{Map, Value, from_value};

/// Typed-config builder driven by a [`DocumentRoot`] (a named-field
/// `#[derive(clapfig::Schema)]` struct, an internally tagged
/// `#[serde(tag = "...")]` enum deriving `Schema`, or
/// `BTreeMap`/`HashMap<String, T>` where `T: Schema`).
///
/// Same surface as [`Builder`](crate::Builder) — `app_name`,
/// `search_paths`, `env_prefix`, `cli_override`, `post_validate`, `load`,
/// `build_resolver`, `handle` — but `load()` returns the typed `C`,
/// `post_validate` receives a typed `&C`, and
/// [`build_resolver`](Self::build_resolver) returns a
/// [`TypedResolver<C>`] whose `resolve_at` yields a typed `C` per call.
pub struct TypedBuilder<C: DocumentRoot> {
    inner: Builder,
    post_validate: Option<TypedHook<C>>,
    _phantom: PhantomData<fn() -> C>,
}

/// The typed post-validate callback, shared between [`TypedBuilder`] and
/// the [`TypedResolver`] it builds.
type TypedHook<C> = Arc<dyn Fn(&C) -> Result<(), String> + Send + Sync>;

/// Run the typed hook, if any, mapping a rejection to
/// [`ClapfigError::PostValidationFailed`]. Only the callback's rejection
/// takes that shape — a typed-deserialize failure stays
/// [`ClapfigError::InvalidValue`] (see [`deserialize_table`]).
fn run_typed_hook<C>(hook: Option<&TypedHook<C>>, typed: &C) -> Result<(), ClapfigError> {
    match hook {
        Some(f) => f(typed).map_err(ClapfigError::PostValidationFailed),
        None => Ok(()),
    }
}

impl<C: DocumentRoot> TypedBuilder<C> {
    pub(crate) fn new() -> Self {
        // Reuse the per-type `Arc<Shape>` cache (object-root derive, or
        // a fresh Arc for HashMap/BTreeMap roots) — one `Arc::clone`
        // per builder construction instead of a full schema-tree clone.
        Self {
            inner: Builder::from_shape(C::shape_arc()),
            post_validate: None,
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

impl<C: DocumentRoot + DeserializeOwned> TypedBuilder<C> {
    /// Post-merge validation hook. Receives the typed `&C`.
    ///
    /// Conceptually the same as
    /// [`Builder::post_validate`](crate::Builder::post_validate), and like
    /// it, calling this method more than once replaces the previous hook.
    /// On the typed surfaces ([`load`](Self::load) and
    /// [`TypedResolver::resolve_at`]) the hook runs on the exact `C`
    /// instance the call returns — the merged [`Map`] is deserialized
    /// once, validated, and handed back, so a non-idempotent
    /// `Deserialize` impl cannot make the validated and returned values
    /// diverge. Only the hook's rejection becomes
    /// [`ClapfigError::PostValidationFailed`]; a typed-deserialize
    /// failure stays [`ClapfigError::InvalidValue`], hook or no hook —
    /// including on the Map-out [`handle`](Self::handle) surface, which
    /// bridges the hook into the inner builder (deserializing a throwaway
    /// `C` to run it) since no typed value is returned there.
    pub fn post_validate<F>(mut self, f: F) -> Self
    where
        F: Fn(&C) -> Result<(), String> + Send + Sync + 'static,
    {
        self.post_validate = Some(Arc::new(f));
        self
    }

    /// Load and resolve the configuration through all layers, returning a
    /// typed `C`. Any [`post_validate`](Self::post_validate) hook runs on
    /// the returned instance.
    pub fn load(self) -> Result<C, ClapfigError> {
        let table = self.inner.load()?;
        let typed = deserialize_table::<C>(table)?;
        run_typed_hook(self.post_validate.as_ref(), &typed)?;
        Ok(typed)
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
        run_typed_hook(self.post_validate.as_ref(), &typed)?;
        Ok((typed, unknowns))
    }

    /// Build a reusable [`TypedResolver<C>`] for tree-walk resolution —
    /// the typed counterpart of
    /// [`Builder::build_resolver`](crate::Builder::build_resolver).
    ///
    /// The captured state, per-call [`SearchPath::Cwd`] /
    /// [`SearchPath::Ancestors`] anchoring, and file caching are the
    /// Map-out resolver's (this wraps one); each
    /// [`resolve_at`](TypedResolver::resolve_at) call additionally
    /// deserializes the merged [`Map`] into a typed `C` — once. Any typed
    /// [`post_validate`](Self::post_validate) hook registered on this
    /// builder moves onto the returned resolver and fires on every
    /// `resolve_at` call, against the instance that call returns. See the
    /// [crate-level "Tree-walk resolution" section](crate#tree-walk-resolution--the-resolver-handle).
    ///
    /// Returns [`ClapfigError::AppNameRequired`] if `.app_name()` was not
    /// called on the builder.
    pub fn build_resolver(self) -> Result<TypedResolver<C>, ClapfigError> {
        Ok(TypedResolver {
            inner: self.inner.build_resolver()?,
            post_validate: self.post_validate,
            _phantom: PhantomData,
        })
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output.
    ///
    /// The action surface is identical to the Map-out path —
    /// `gen | schema | get | list | set | unset` all delegate. A typed
    /// [`post_validate`](Self::post_validate) hook still guards the
    /// merged `get`/`list` views: it is bridged into the Map-out builder
    /// (deserializing a `C` to run it) since no typed value is returned
    /// here. A deserialize failure on that throwaway `C` stays
    /// [`ClapfigError::InvalidValue`]; only the hook's own rejection
    /// becomes [`ClapfigError::PostValidationFailed`].
    pub fn handle(self, action: &ConfigAction) -> Result<ConfigResult, ClapfigError>
    where
        C: 'static,
    {
        self.into_inner().handle(action)
    }

    /// Dispatch a [`ConfigAction`] and print the result.
    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError>
    where
        C: 'static,
    {
        self.into_inner().handle_and_print(action)
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output as a
    /// `String`.
    pub fn handle_to_string(self, action: &ConfigAction) -> Result<String, ClapfigError>
    where
        C: 'static,
    {
        self.into_inner().handle_to_string(action)
    }

    /// Collapse into the Map-out builder for `handle` dispatch, bridging
    /// any typed hook into a Map-level one (the merged `get`/`list` views
    /// resolve through the Map pipeline, which cannot call a typed
    /// closure directly). Deserialize failures stay
    /// [`ClapfigError::InvalidValue`]; only the typed hook's own
    /// rejection becomes [`ClapfigError::PostValidationFailed`].
    fn into_inner(self) -> Builder
    where
        C: 'static,
    {
        match self.post_validate {
            Some(f) => self.inner.post_validate_err(move |t: &Map| {
                let typed = deserialize_table::<C>(t.clone())?;
                run_typed_hook(Some(&f), &typed)
            }),
            None => self.inner,
        }
    }
}

/// Typed tree-walk resolution handle — the typed-out counterpart of
/// [`Resolver`](crate::Resolver), built by
/// [`TypedBuilder::build_resolver`].
///
/// Wraps the Map-out resolver, so anchoring semantics and the
/// per-resolver file cache (including its no-mtime-check contract) are
/// identical; each call adds one final `Map → C` deserialize through the
/// value model's serde bridge, and then runs the typed
/// [`post_validate`](TypedBuilder::post_validate) hook (carried on this
/// resolver, not the wrapped one) on that same instance.
pub struct TypedResolver<C> {
    inner: Resolver,
    post_validate: Option<TypedHook<C>>,
    _phantom: PhantomData<fn() -> C>,
}

impl<C: DocumentRoot + DeserializeOwned> TypedResolver<C> {
    /// Resolve the configuration anchored at `start_dir`, returning a
    /// typed `C`. See [`Resolver::resolve_at`](crate::Resolver::resolve_at)
    /// for the anchoring and caching semantics. The merged [`Map`] is
    /// deserialized exactly once, and any typed
    /// [`post_validate`](TypedBuilder::post_validate) hook runs on the
    /// instance this call returns.
    pub fn resolve_at(&self, start_dir: impl AsRef<std::path::Path>) -> Result<C, ClapfigError> {
        let typed = deserialize_table::<C>(self.inner.resolve_at(start_dir)?)?;
        run_typed_hook(self.post_validate.as_ref(), &typed)?;
        Ok(typed)
    }

    /// Same as [`resolve_at`](Self::resolve_at) but also returns any keys
    /// the [`on_unknown_key`](TypedBuilder::on_unknown_key) callback
    /// elected to
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    pub fn resolve_at_with_unknowns(
        &self,
        start_dir: impl AsRef<std::path::Path>,
    ) -> Result<(C, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        let (table, unknowns) = self.inner.resolve_at_with_unknowns(start_dir)?;
        let typed = deserialize_table::<C>(table)?;
        run_typed_hook(self.post_validate.as_ref(), &typed)?;
        Ok((typed, unknowns))
    }

    /// Number of files currently held in the wrapped resolver's cache.
    /// Intended for tests and diagnostics; production code should not
    /// branch on this.
    #[doc(hidden)]
    pub fn cache_size(&self) -> usize {
        self.inner.cache_size()
    }
}

fn deserialize_table<C: DeserializeOwned>(table: Map) -> Result<C, ClapfigError> {
    // The value model's serde bridge carries datetimes through its
    // private marker struct, so this deserializes directly — no
    // serialize-reparse round trip (the hack the owned model retired).
    from_value(Value::Map(table))
        .map_err(|e| ClapfigError::invalid_value("<merged>", e.to_string()))
}
