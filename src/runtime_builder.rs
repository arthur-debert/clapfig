//! Runtime-path builder API, parallel to [`crate::builder::ClapfigBuilder`].
//!
//! Entry point: [`crate::Clapfig::runtime(schema)`](crate::Clapfig::runtime).
//! The runtime builder exposes the same surface as the static `ClapfigBuilder`
//! — discovery, persistence, env, URL, CLI overrides, post-validation,
//! tree-walk resolution — but produces a `toml::Table` instead of a typed
//! struct.
//!
//! The duplication with [`crate::builder`] is deliberate for Phase 2: it
//! keeps the static path's public surface byte-identical, and most methods
//! here are one-line forwarders. Phase 4 can revisit factoring the shared
//! `BuilderConfig` if the maintenance cost matters.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use toml::{Table, Value};

use crate::error::ClapfigError;
use crate::file;
use crate::flatten;
use crate::ops::{self, ConfigResult};
use crate::overrides;
use crate::persist;
use crate::resolve::{self, ResolveInput};
use crate::runtime::Schema;
use crate::runtime_spec::DynamicSpec;
use crate::spec::SchemaRef;
use crate::strict::{StrictnessOverrides, UnknownKeyHook};
use crate::types::{ConfigAction, Layer, SearchMode, SearchPath};

/// Post-merge validation hook for the runtime path: receives the merged
/// `toml::Table` (the runtime equivalent of the static path's `&C`).
pub(crate) type RuntimePostValidateHook = Box<dyn Fn(&Table) -> Result<(), String> + Send + Sync>;

/// Builder for runtime-defined configurations.
///
/// Same surface as [`ClapfigBuilder`](crate::ClapfigBuilder<C>), but the
/// schema is supplied at runtime (via [`crate::Clapfig::runtime`]) and the
/// loaded value is a `toml::Table` rather than a typed struct.
pub struct RuntimeBuilder {
    spec: Arc<DynamicSpec>,
    app_name: Option<String>,
    file_name: Option<String>,
    search_paths: Option<Vec<SearchPath>>,
    search_mode: SearchMode,
    persist_scopes: Vec<(String, SearchPath)>,
    env_prefix: Option<String>,
    env_enabled: bool,
    strict: bool,
    normalize_keys: bool,
    #[cfg(feature = "url")]
    url_overrides: Vec<(String, Value)>,
    cli_overrides: Vec<(String, Value)>,
    layer_order: Option<Vec<Layer>>,
    post_validate: Option<RuntimePostValidateHook>,
    strict_at_overrides: Vec<(String, bool)>,
    unknown_key_hook: Option<UnknownKeyHook>,
}

impl RuntimeBuilder {
    pub(crate) fn new(schema: Schema) -> Self {
        Self::from_spec(Arc::new(DynamicSpec::new(schema)))
    }

    /// Construct a builder reusing an already-`Arc<Schema>`-cached schema
    /// (e.g. the per-type cache the `clapfig::Schema` derive maintains).
    /// Skips the per-builder clone of the schema tree that
    /// [`new`](Self::new) performs.
    pub(crate) fn from_arc(schema: Arc<Schema>) -> Self {
        Self::from_spec(Arc::new(DynamicSpec::from_arc(schema)))
    }

    fn from_spec(spec: Arc<DynamicSpec>) -> Self {
        Self {
            spec,
            app_name: None,
            file_name: None,
            search_paths: None,
            search_mode: SearchMode::default(),
            persist_scopes: Vec::new(),
            env_prefix: None,
            env_enabled: true,
            strict: true,
            normalize_keys: false,
            #[cfg(feature = "url")]
            url_overrides: Vec::new(),
            cli_overrides: Vec::new(),
            layer_order: None,
            post_validate: None,
            strict_at_overrides: Vec::new(),
            unknown_key_hook: None,
        }
    }

    /// Set the application name. See
    /// [`ClapfigBuilder::app_name`](crate::ClapfigBuilder::app_name).
    pub fn app_name(mut self, name: &str) -> Self {
        self.app_name = Some(name.to_string());
        self
    }

    /// Override the config file name. See
    /// [`ClapfigBuilder::file_name`](crate::ClapfigBuilder::file_name).
    pub fn file_name(mut self, name: &str) -> Self {
        self.file_name = Some(name.to_string());
        self
    }

    /// Replace the default search paths entirely. See
    /// [`ClapfigBuilder::search_paths`](crate::ClapfigBuilder::search_paths).
    pub fn search_paths(mut self, paths: Vec<SearchPath>) -> Self {
        self.search_paths = Some(paths);
        self
    }

    /// Append a single search path. See
    /// [`ClapfigBuilder::add_search_path`](crate::ClapfigBuilder::add_search_path).
    pub fn add_search_path(mut self, path: SearchPath) -> Self {
        self.search_paths
            .get_or_insert_with(|| vec![SearchPath::Platform])
            .push(path);
        self
    }

    /// Set the search mode (`Merge` vs `FirstMatch`). See
    /// [`ClapfigBuilder::search_mode`](crate::ClapfigBuilder::search_mode).
    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = mode;
        self
    }

    /// Register a named persist scope for `config set`/`unset`. See
    /// [`ClapfigBuilder::persist_scope`](crate::ClapfigBuilder::persist_scope).
    pub fn persist_scope(mut self, name: &str, path: SearchPath) -> Self {
        self.persist_scopes.push((name.to_string(), path));
        self
    }

    /// Override the env var prefix. See
    /// [`ClapfigBuilder::env_prefix`](crate::ClapfigBuilder::env_prefix).
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Disable env loading entirely. See
    /// [`ClapfigBuilder::no_env`](crate::ClapfigBuilder::no_env).
    pub fn no_env(mut self) -> Self {
        self.env_enabled = false;
        self
    }

    /// Set the whole-resolution strictness default. See
    /// [`ClapfigBuilder::strict`](crate::ClapfigBuilder::strict) and the
    /// [cascading strictness section](crate#cascading-strictness) of the
    /// crate docs for how this composes with `strict_at` and
    /// `on_unknown_key`.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Set per-section strictness for the dotted path `path`. See
    /// [`ClapfigBuilder::strict_at`](crate::ClapfigBuilder::strict_at) for
    /// the full cascade semantics — same rule, validated against the
    /// runtime schema instead of `C::META`.
    pub fn strict_at(mut self, path: &str, strict: bool) -> Self {
        self.strict_at_overrides.push((path.to_string(), strict));
        self
    }

    /// Register a per-key callback for cascade-rejected unknown keys. See
    /// [`ClapfigBuilder::on_unknown_key`](crate::ClapfigBuilder::on_unknown_key)
    /// for the full decision chain.
    pub fn on_unknown_key<F>(mut self, callback: F) -> Self
    where
        F: Fn(&crate::UnknownKeyContext<'_>) -> crate::UnknownKeyDecision + Send + Sync + 'static,
    {
        self.unknown_key_hook = Some(std::sync::Arc::new(callback));
        self
    }

    /// Convenience: "accept dotted, reject bare" at a dotted-path
    /// subtree. See
    /// [`ClapfigBuilder::accept_dotted_extension_keys_in`](crate::ClapfigBuilder::accept_dotted_extension_keys_in)
    /// for the full semantics — same rule, runtime-path schema.
    pub fn accept_dotted_extension_keys_in(
        mut self,
        path: &str,
        decision: crate::UnknownKeyDecision,
    ) -> Self {
        self.unknown_key_hook = Some(crate::strict::dotted_extension_callback(
            path.to_string(),
            decision,
        ));
        self
    }

    /// Accept kebab-case keys in config files and CLI/URL overrides. See
    /// [`ClapfigBuilder::normalize_keys`](crate::ClapfigBuilder::normalize_keys).
    pub fn normalize_keys(mut self, normalize: bool) -> Self {
        self.normalize_keys = normalize;
        self
    }

    /// Set a custom layer merge order. See
    /// [`ClapfigBuilder::layer_order`](crate::ClapfigBuilder::layer_order).
    pub fn layer_order(mut self, order: Vec<Layer>) -> Self {
        self.layer_order = Some(order);
        self
    }

    /// Post-merge validation hook. Receives the merged `&toml::Table` (the
    /// runtime analogue of the static path's `&C`).
    pub fn post_validate<F>(mut self, f: F) -> Self
    where
        F: Fn(&Table) -> Result<(), String> + Send + Sync + 'static,
    {
        self.post_validate = Some(Box::new(f));
        self
    }

    /// Add a URL query string as a config layer. See
    /// [`ClapfigBuilder::url_query`](crate::ClapfigBuilder::url_query).
    #[cfg(feature = "url")]
    pub fn url_query(mut self, query: &str) -> Self {
        self.url_overrides
            .extend(crate::url::query_to_overrides(query));
        self
    }

    /// Add a single CLI override. See
    /// [`ClapfigBuilder::cli_override`](crate::ClapfigBuilder::cli_override).
    pub fn cli_override<V: Into<Value>>(mut self, key: &str, value: Option<V>) -> Self {
        if let Some(v) = value {
            self.cli_overrides.push((key.to_string(), v.into()));
        }
        self
    }

    /// Match a serializable struct's fields against the runtime schema's
    /// keys (same auto-matching behavior as the static path's
    /// `cli_overrides_from`).
    pub fn cli_overrides_from<S: Serialize>(mut self, source: &S) -> Self {
        let pairs = flatten::flatten(source)
            .expect("clapfig: failed to flatten CLI source for auto-matching");
        let valid = overrides::valid_keys(SchemaRef::from_dynamic(&self.spec.schema));
        for (key, value) in pairs {
            if let Some(v) = value
                && valid.contains(&key)
            {
                self.cli_overrides.push((key, v));
            }
        }
        self
    }

    fn effective_app_name(&self) -> Result<&str, ClapfigError> {
        self.app_name
            .as_deref()
            .ok_or(ClapfigError::AppNameRequired)
    }

    fn effective_file_name(&self) -> Result<String, ClapfigError> {
        if let Some(name) = &self.file_name {
            return Ok(name.clone());
        }
        let app = self.effective_app_name()?;
        Ok(format!("{app}.toml"))
    }

    fn effective_search_paths(&self) -> Vec<SearchPath> {
        let mut paths = if let Some(paths) = &self.search_paths {
            paths.clone()
        } else {
            vec![SearchPath::Platform]
        };
        for (_, scope_path) in &self.persist_scopes {
            if !paths.contains(scope_path) {
                paths.push(scope_path.clone());
            }
        }
        paths
    }

    fn effective_env_prefix(&self) -> Result<Option<String>, ClapfigError> {
        if !self.env_enabled {
            return Ok(None);
        }
        if let Some(prefix) = &self.env_prefix {
            return Ok(Some(prefix.clone()));
        }
        let app = self.effective_app_name()?;
        Ok(Some(app.to_uppercase()))
    }

    /// Build a reusable [`RuntimeResolver`] that captures the current
    /// builder state and can be called repeatedly with
    /// [`resolve_at(dir)`](RuntimeResolver::resolve_at). Runtime-path
    /// analogue of
    /// [`ClapfigBuilder::build_resolver`](crate::ClapfigBuilder::build_resolver);
    /// see that method for the full design rationale.
    pub fn build_resolver(self) -> Result<RuntimeResolver, ClapfigError> {
        let app_name = self.effective_app_name()?.to_string();
        let file_name = self.effective_file_name()?;
        let search_paths = self.effective_search_paths();
        let env_prefix = self.effective_env_prefix()?;
        let env_vars = if env_prefix.is_some() {
            std::env::vars().collect()
        } else {
            Vec::new()
        };

        // Validate `strict_at` paths against the runtime schema and merge
        // with the schema's own per-node `strict` settings into a single
        // cascade map. `build_strict_overrides` lives in `builder` so the
        // static and runtime paths share identical typo-protection rules.
        let strict_overrides = crate::builder::build_strict_overrides(
            &self.strict_at_overrides,
            self.normalize_keys,
            SchemaRef::from_dynamic(&self.spec.schema),
        )?;

        Ok(RuntimeResolver {
            spec: self.spec,
            app_name,
            file_name,
            search_paths,
            search_mode: self.search_mode,
            env_prefix,
            env_vars,
            strict_default: self.strict,
            strict_overrides,
            unknown_key_hook: self.unknown_key_hook,
            normalize_keys: self.normalize_keys,
            #[cfg(feature = "url")]
            url_overrides: self.url_overrides,
            cli_overrides: self.cli_overrides,
            layer_order: self.layer_order,
            post_validate: self.post_validate.map(Arc::new),
            file_cache: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Load and resolve the configuration through all layers. Runtime-path
    /// analogue of [`ClapfigBuilder::load`](crate::ClapfigBuilder::load),
    /// returning a [`toml::Table`] instead of a typed `C`.
    pub fn load(self) -> Result<Table, ClapfigError> {
        let start_dir = std::env::current_dir().map_err(|e| ClapfigError::IoError {
            path: PathBuf::from("."),
            source: e,
        })?;
        self.build_resolver()?.resolve_at(start_dir)
    }

    /// Same as [`load`](Self::load) but also returns any keys the
    /// [`on_unknown_key`](Self::on_unknown_key) callback elected to
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    /// The list is empty when no callback is registered or no key opts in
    /// — this is the direct fix for callers that currently smuggle a
    /// shared `Vec` through closure captures to get the same data out.
    pub fn load_with_unknowns(
        self,
    ) -> Result<(Table, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        let start_dir = std::env::current_dir().map_err(|e| ClapfigError::IoError {
            path: PathBuf::from("."),
            source: e,
        })?;
        self.build_resolver()?.resolve_at_with_unknowns(start_dir)
    }

    /// Dispatch a [`ConfigAction`] and print the result. Runtime-path
    /// analogue of
    /// [`ClapfigBuilder::handle_and_print`](crate::ClapfigBuilder::handle_and_print).
    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError> {
        let result = self.handle(action)?;
        print!("{result}");
        Ok(())
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output as a
    /// `String`. Runtime-path analogue of
    /// [`ClapfigBuilder::handle_to_string`](crate::ClapfigBuilder::handle_to_string).
    pub fn handle_to_string(self, action: &ConfigAction) -> Result<String, ClapfigError> {
        self.handle(action).map(|r| r.to_string())
    }

    fn resolve_scope_persist_path(&self, scope: Option<&str>) -> Result<PathBuf, ClapfigError> {
        if self.persist_scopes.is_empty() {
            return Err(ClapfigError::NoPersistPath);
        }
        let app_name = self.effective_app_name()?;
        let file_name = self.effective_file_name()?;
        let (_, search_path) = match scope {
            None => &self.persist_scopes[0],
            Some(name) => self
                .persist_scopes
                .iter()
                .find(|(n, _)| n == name)
                .ok_or_else(|| ClapfigError::UnknownScope {
                    scope: name.to_string(),
                    available: self.persist_scopes.iter().map(|(n, _)| n.clone()).collect(),
                })?,
        };
        file::resolve_persist_path(search_path, &file_name, app_name)
    }

    /// Dispatch a [`ConfigAction`] against the runtime schema.
    ///
    /// Returns the same [`ConfigResult`] enum the static path produces, so
    /// downstream rendering / printing code is shared.
    pub fn handle(self, action: &ConfigAction) -> Result<ConfigResult, ClapfigError> {
        match action {
            ConfigAction::List { scope } => match scope {
                None => {
                    let table = self.load()?;
                    Ok(list_from_table(&table))
                }
                Some(name) => {
                    let path = self.resolve_scope_persist_path(Some(name))?;
                    ops::list_scope_file(&path)
                }
            },
            ConfigAction::Gen { output } => {
                let template =
                    ops::generate_template_from_runtime(&self.spec.schema, self.normalize_keys);
                match output {
                    Some(path) => {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| ClapfigError::IoError {
                                path: parent.to_path_buf(),
                                source: e,
                            })?;
                        }
                        std::fs::write(path, &template).map_err(|e| ClapfigError::IoError {
                            path: path.clone(),
                            source: e,
                        })?;
                        Ok(ConfigResult::TemplateWritten { path: path.clone() })
                    }
                    None => Ok(ConfigResult::Template(template)),
                }
            }
            ConfigAction::Schema { output } => {
                let value = crate::schema::generate_schema_from_ref(SchemaRef::from_dynamic(
                    &self.spec.schema,
                ));
                let schema = serde_json::to_string_pretty(&value)
                    .expect("serde_json::Value serialization is infallible");
                match output {
                    Some(path) => {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| ClapfigError::IoError {
                                path: parent.to_path_buf(),
                                source: e,
                            })?;
                        }
                        std::fs::write(path, &schema).map_err(|e| ClapfigError::IoError {
                            path: path.clone(),
                            source: e,
                        })?;
                        Ok(ConfigResult::SchemaWritten { path: path.clone() })
                    }
                    None => Ok(ConfigResult::Schema(schema)),
                }
            }
            ConfigAction::Get { key, scope } => match scope {
                None => {
                    let spec = Arc::clone(&self.spec);
                    let table = self.load()?;
                    get_from_table(&spec.schema, &table, key)
                }
                Some(name) => {
                    let path = self.resolve_scope_persist_path(Some(name))?;
                    get_scope_runtime(&self.spec.schema, &path, key)
                }
            },
            ConfigAction::Set { key, value, scope } => {
                let path = self.resolve_scope_persist_path(scope.as_deref())?;
                persist::persist_value_runtime(&self.spec.schema, &path, key, value)
            }
            ConfigAction::Unset { key, scope } => {
                let path = self.resolve_scope_persist_path(scope.as_deref())?;
                crate::persist::unset_value(&path, key)
            }
        }
    }
}

/// Reusable runtime resolution handle. Parallel to [`Resolver<C>`](crate::Resolver).
pub struct RuntimeResolver {
    spec: Arc<DynamicSpec>,
    app_name: String,
    file_name: String,
    search_paths: Vec<SearchPath>,
    search_mode: SearchMode,
    env_prefix: Option<String>,
    env_vars: Vec<(String, String)>,
    strict_default: bool,
    strict_overrides: StrictnessOverrides,
    unknown_key_hook: Option<UnknownKeyHook>,
    normalize_keys: bool,
    #[cfg(feature = "url")]
    url_overrides: Vec<(String, Value)>,
    cli_overrides: Vec<(String, Value)>,
    layer_order: Option<Vec<Layer>>,
    post_validate: Option<Arc<RuntimePostValidateHook>>,
    file_cache: Mutex<std::collections::HashMap<PathBuf, String>>,
}

impl RuntimeResolver {
    pub fn resolve_at(
        &self,
        start_dir: impl AsRef<std::path::Path>,
    ) -> Result<Table, ClapfigError> {
        let start_dir = start_dir.as_ref();
        let absolute = if start_dir.is_absolute() {
            start_dir.to_path_buf()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(start_dir),
                Err(e) => {
                    return Err(ClapfigError::IoError {
                        path: start_dir.to_path_buf(),
                        source: e,
                    });
                }
            }
        };
        let normalized = std::fs::canonicalize(&absolute).unwrap_or(absolute);

        let dirs = file::expand_search_paths(&self.search_paths, &self.app_name, &normalized);
        let files = self.load_files_cached(&dirs)?;

        let input = ResolveInput {
            spec: self.spec.as_ref(),
            files,
            env_vars: self.env_vars.clone(),
            env_prefix: self.env_prefix.clone(),
            #[cfg(feature = "url")]
            url_overrides: self.url_overrides.clone(),
            cli_overrides: self.cli_overrides.clone(),
            strict_default: self.strict_default,
            strict_overrides: self.strict_overrides.clone(),
            unknown_key_hook: self.unknown_key_hook.clone(),
            normalize_keys: self.normalize_keys,
            layer_order: self.layer_order.clone(),
        };

        let (table, _unknowns) = resolve::resolve(input)?;
        if let Some(hook) = self.post_validate.as_ref() {
            hook(&table).map_err(ClapfigError::PostValidationFailed)?;
        }
        Ok(table)
    }

    /// Same as [`resolve_at`](Self::resolve_at) but also returns any keys
    /// the [`on_unknown_key`](crate::ClapfigBuilder::on_unknown_key)
    /// callback elected to [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    /// Runtime-path analogue of
    /// [`Resolver::resolve_at_with_unknowns`](crate::Resolver::resolve_at_with_unknowns).
    pub fn resolve_at_with_unknowns(
        &self,
        start_dir: impl AsRef<std::path::Path>,
    ) -> Result<(Table, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        let start_dir = start_dir.as_ref();
        let absolute = if start_dir.is_absolute() {
            start_dir.to_path_buf()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(start_dir),
                Err(e) => {
                    return Err(ClapfigError::IoError {
                        path: start_dir.to_path_buf(),
                        source: e,
                    });
                }
            }
        };
        let normalized = std::fs::canonicalize(&absolute).unwrap_or(absolute);

        let dirs = file::expand_search_paths(&self.search_paths, &self.app_name, &normalized);
        let files = self.load_files_cached(&dirs)?;

        let input = ResolveInput {
            spec: self.spec.as_ref(),
            files,
            env_vars: self.env_vars.clone(),
            env_prefix: self.env_prefix.clone(),
            #[cfg(feature = "url")]
            url_overrides: self.url_overrides.clone(),
            cli_overrides: self.cli_overrides.clone(),
            strict_default: self.strict_default,
            strict_overrides: self.strict_overrides.clone(),
            unknown_key_hook: self.unknown_key_hook.clone(),
            normalize_keys: self.normalize_keys,
            layer_order: self.layer_order.clone(),
        };

        let (table, unknowns) = resolve::resolve(input)?;
        if let Some(hook) = self.post_validate.as_ref() {
            hook(&table).map_err(ClapfigError::PostValidationFailed)?;
        }
        Ok((table, unknowns))
    }

    fn load_files_cached(&self, dirs: &[PathBuf]) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
        match self.search_mode {
            SearchMode::Merge => {
                let mut out = Vec::new();
                for dir in dirs {
                    let path = dir.join(&self.file_name);
                    if let Some(contents) = self.read_cached(&path)? {
                        out.push((path, contents));
                    }
                }
                Ok(out)
            }
            SearchMode::FirstMatch => {
                for dir in dirs.iter().rev() {
                    let path = dir.join(&self.file_name);
                    if let Some(contents) = self.read_cached(&path)? {
                        return Ok(vec![(path, contents)]);
                    }
                }
                Ok(Vec::new())
            }
        }
    }

    /// Number of files currently held in the resolver's cache. Intended for
    /// tests and diagnostics; production code should not branch on this.
    /// Parallel to [`Resolver::cache_size`](crate::Resolver::cache_size).
    #[doc(hidden)]
    pub fn cache_size(&self) -> usize {
        self.file_cache
            .lock()
            .expect("file_cache mutex poisoned")
            .len()
    }

    fn read_cached(&self, path: &std::path::Path) -> Result<Option<String>, ClapfigError> {
        {
            let cache = self.file_cache.lock().expect("file_cache mutex poisoned");
            if let Some(cached) = cache.get(path) {
                return Ok(Some(cached.clone()));
            }
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut cache = self.file_cache.lock().expect("file_cache mutex poisoned");
                cache.insert(path.to_path_buf(), contents.clone());
                Ok(Some(contents))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ClapfigError::IoError {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }
}

/// Render every leaf in a resolved runtime table as `dotted.key = value`
/// entries, matching what the static path's `ops::list_values` produces.
fn list_from_table(table: &Table) -> ConfigResult {
    let mut entries = Vec::new();
    flatten_table(table, "", &mut entries);
    ConfigResult::Listing { entries }
}

fn flatten_table(table: &Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(t) => flatten_table(t, &full, out),
            _ => out.push((full, format_runtime_value(value))),
        }
    }
}

fn format_runtime_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(_) | Value::Table(_) => {
            toml::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
        }
    }
}

fn get_from_table(schema: &Schema, table: &Table, key: &str) -> Result<ConfigResult, ClapfigError> {
    let value = ops::table_get(table, key).ok_or_else(|| ClapfigError::KeyNotFound(key.into()))?;
    let doc = crate::meta::doc_for_runtime(schema, key).unwrap_or_default();
    Ok(ConfigResult::KeyValue {
        key: key.into(),
        value: format_runtime_value(value),
        doc,
    })
}

fn get_scope_runtime(
    schema: &Schema,
    file_path: &std::path::Path,
    key: &str,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClapfigError::KeyNotFound(key.into()));
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let table: Table = content
        .parse()
        .map_err(|e: toml::de::Error| ClapfigError::ParseError {
            path: file_path.to_path_buf(),
            source: Box::new(e),
            source_text: Some(Arc::from(content.as_str())),
        })?;

    let value = ops::table_get(&table, key).ok_or_else(|| ClapfigError::KeyNotFound(key.into()))?;
    let doc = crate::meta::doc_for_runtime(schema, key).unwrap_or_default();
    Ok(ConfigResult::KeyValue {
        key: key.into(),
        value: format_runtime_value(value),
        doc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Field as RtField;
    use crate::{Clapfig, ConfigAction};
    use std::fs;
    use tempfile::TempDir;

    fn demo_schema() -> Schema {
        Schema::object("App")
            .doc("Demo runtime schema")
            .field(
                "host",
                RtField::string().doc("App host").default("localhost"),
            )
            .field(
                "port",
                RtField::integer().doc("Port number").default(8080i64),
            )
            .field(
                "level",
                RtField::enum_of(["debug", "info", "warn", "error"])
                    .doc("Log verbosity")
                    .default("info"),
            )
            .nested(
                "db",
                Schema::object("Db")
                    .doc("Database settings")
                    .field("url", RtField::string().optional())
                    .field("pool_size", RtField::integer().default(5i64)),
            )
            .build()
    }

    // --- file + defaults ---

    #[test]
    fn load_uses_defaults_when_no_file() {
        let dir = TempDir::new().unwrap();
        let table = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("level"), Some(&Value::String("info".into())));
        let db = table.get("db").and_then(Value::as_table).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(5)));
    }

    #[test]
    fn load_file_overrides_defaults() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "port = 9090\n[db]\nurl = \"pg://prod\"\n",
        )
        .unwrap();

        let table = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("port"), Some(&Value::Integer(9090)));
        let db = table.get("db").and_then(Value::as_table).unwrap();
        assert_eq!(db.get("url"), Some(&Value::String("pg://prod".into())));
    }

    // --- env + CLI override ---

    #[test]
    fn load_env_overrides_file() {
        // Unique env var name keeps this test isolated from parallel runs.
        const KEY: &str = "CLAPFIG_RT_BUILDER_ENV_TEST__PORT";
        unsafe { std::env::set_var(KEY, "7000") };

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 9000\n").unwrap();

        let table = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .env_prefix("CLAPFIG_RT_BUILDER_ENV_TEST")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .load()
            .unwrap();

        unsafe { std::env::remove_var(KEY) };
        assert_eq!(table.get("port"), Some(&Value::Integer(7000)));
    }

    #[test]
    fn cli_override_wins() {
        let dir = TempDir::new().unwrap();
        let table = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_override("port", Some(11111i64))
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(11111)));
    }

    // --- strict / unknown keys ---

    #[test]
    fn strict_rejects_unknown_top_level_with_line_number() {
        let dir = TempDir::new().unwrap();
        let source = "port = 8080\ntypo = 1\n";
        fs::write(dir.path().join("demo.toml"), source).unwrap();

        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();

        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("unknown keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "typo");
        assert_eq!(keys[0].line, 2);
    }

    // --- enum validation ---

    #[test]
    fn rejects_out_of_set_enum_value_at_load() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "level = \"garbage\"\n").unwrap();

        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();

        match result {
            Err(ClapfigError::InvalidValue { key, reason }) => {
                assert_eq!(key, "level");
                assert!(reason.contains("not in allowed set"));
            }
            other => panic!("expected InvalidValue(level), got {other:?}"),
        }
    }

    #[test]
    fn rejects_out_of_set_enum_value_on_set() {
        let dir = TempDir::new().unwrap();
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "level".into(),
                value: "garbage".into(),
                scope: None,
            });

        assert!(matches!(result, Err(ClapfigError::InvalidValue { .. })));
        // File must not have been written.
        assert!(!dir.path().join("demo.toml").exists());
    }

    // --- required-field check ---

    #[test]
    fn required_field_without_default_errors() {
        // Build a schema with a required field that has no default.
        let schema = Schema::object("Req")
            .field("name", RtField::string()) // required
            .build();
        let dir = TempDir::new().unwrap();

        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();

        match result {
            Err(ClapfigError::MissingRequired { key }) => assert_eq!(key, "name"),
            other => panic!("expected MissingRequired(name), got {other:?}"),
        }
    }

    // --- post_validate hook ---

    #[test]
    fn post_validate_receives_merged_table() {
        let dir = TempDir::new().unwrap();
        let seen = Arc::new(Mutex::new(0i64));
        let seen_clone = Arc::clone(&seen);

        let _ = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .post_validate(move |t: &Table| {
                *seen_clone.lock().unwrap() = t.get("port").and_then(Value::as_integer).unwrap();
                Ok(())
            })
            .load()
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), 8080);
    }

    #[test]
    fn post_validate_err_propagates() {
        let dir = TempDir::new().unwrap();
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .post_validate(|_| Err("nope".into()))
            .load();
        match result {
            Err(ClapfigError::PostValidationFailed(msg)) => assert_eq!(msg, "nope"),
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    // --- handle: gen / schema / get / list / set / unset ---

    #[test]
    fn handle_gen_emits_local_leaves_before_nested_sections() {
        // Regression: TOML rule — once `[section]` opens, every following
        // key belongs to that section. A sibling leaf declared after a
        // nested field in the schema must still render under its parent,
        // not inside the previous section. The fix reorders the emitter so
        // local leaves render first, then sections.
        let schema = Schema::object("Top")
            .field("first", RtField::string().default("a"))
            .nested(
                "inner",
                Schema::object("Inner").field("x", RtField::integer().default(1i64)),
            )
            .field("second", RtField::string().default("b"))
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();
        let t = match result {
            ConfigResult::Template(t) => t,
            other => panic!("expected Template, got {other:?}"),
        };
        // Re-parse the output as TOML and verify `second` is at the root,
        // not inside `[inner]`.
        let parsed: toml::Table = t.parse().unwrap();
        assert!(parsed.get("first").is_some(), "first must be at root:\n{t}");
        assert!(
            parsed.get("second").is_some(),
            "second leaked into [inner] (template ordering bug):\n{t}"
        );
        let inner = parsed.get("inner").and_then(|v| v.as_table()).unwrap();
        assert!(
            inner.get("second").is_none(),
            "second must not be under inner"
        );
    }

    #[test]
    fn handle_gen_renders_template_with_doc_comments_and_enum_set() {
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();

        match result {
            ConfigResult::Template(t) => {
                assert!(t.contains("# Demo runtime schema"));
                assert!(t.contains("host = \"localhost\""));
                assert!(t.contains("port = 8080"));
                assert!(t.contains("# Allowed: \"debug\" | \"info\" | \"warn\" | \"error\""));
                assert!(t.contains("level = \"info\""));
                assert!(t.contains("[db]"));
            }
            other => panic!("expected Template, got {other:?}"),
        }
    }

    #[test]
    fn handle_gen_renders_value_leaf_with_accepts_hint() {
        // LeafType::Value is the escape hatch for keys whose value can
        // take multiple incompatible shapes (issue #47). The template
        // must surface this in the doc-comment area so the user knows
        // the leaf is intentionally unconstrained.
        let schema = Schema::object("Top")
            .field(
                "rule",
                RtField::value().doc("Either a severity string or [severity, options]."),
            )
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();
        match result {
            ConfigResult::Template(t) => {
                assert!(t.contains("# Either a severity string"));
                assert!(t.contains("# Accepts: any TOML value"));
                assert!(t.contains("#rule = \"\""));
            }
            other => panic!("expected Template, got {other:?}"),
        }
    }

    #[test]
    fn handle_schema_value_leaf_omits_type_constraint() {
        // JSON Schema convention for unconstrained: omit `type` entirely.
        // A LeafType::Value field should appear in the schema with its
        // description but no type/enum/etc. constraint.
        let schema = Schema::object("Top")
            .field("rule", RtField::value().doc("Any TOML value."))
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Schema { output: None })
            .unwrap();
        match result {
            ConfigResult::Schema(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                let rule = &v["properties"]["rule"];
                assert!(rule.is_object(), "rule property missing");
                assert!(
                    rule.get("type").is_none(),
                    "Value leaves must have no `type` key (JSON Schema convention for unconstrained); got {rule}"
                );
                assert_eq!(rule["description"], "Any TOML value.");
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn value_leaf_accepts_any_shape_at_load() {
        // The whole point of LeafType::Value: don't reject either the
        // bare-string or the array-with-options shape on the same key.
        let dir = TempDir::new().unwrap();
        let toml_path = dir.path().join("demo.toml");
        std::fs::write(
            &toml_path,
            "[rules]\nmissing_footnote = \"warn\"\nbad_columns = [\"warn\", { max = 80 }]\n",
        )
        .unwrap();

        let schema = Schema::object("Top")
            .nested(
                "rules",
                Schema::object("Rules")
                    .strict(false)
                    .field("missing_footnote", RtField::value())
                    .field("bad_columns", RtField::value()),
            )
            .build();

        let table = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        let rules = table["rules"].as_table().unwrap();
        assert_eq!(rules["missing_footnote"].as_str(), Some("warn"));
        assert!(rules["bad_columns"].as_array().is_some());
    }

    #[test]
    fn handle_schema_does_not_mark_array_of_required() {
        // Regression: `DynamicSpec::finalize` accepts an absent ArrayOf as
        // the empty list. The JSON Schema must agree — marking the
        // property required would reject configs clapfig itself accepts.
        let schema = Schema::object("Top")
            .field("name", RtField::string().default("x"))
            .array_of(
                "plugins",
                Schema::object("Plugin").field("id", RtField::string()),
            )
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Schema { output: None })
            .unwrap();
        match result {
            ConfigResult::Schema(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                let required = v["required"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|x| x.as_str().unwrap().to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                assert!(
                    !required.contains(&"plugins".to_string()),
                    "plugins must not be in required: {required:?}"
                );
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    // --- Field::MapOf (issue #54 item 2) ---

    fn map_of_schema() -> Schema {
        Schema::object("Cfg")
            .map_of(
                "plugins",
                Schema::object("Plugin")
                    .field("enabled", RtField::boolean().default(false))
                    .field("severity", RtField::string()),
            )
            .build()
    }

    #[test]
    fn map_of_accepts_user_keyed_entries() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[plugins.audit]\nseverity = \"warn\"\n\n[plugins.fmt]\nenabled = true\nseverity = \"error\"\n",
        )
        .unwrap();
        let table = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        let plugins = table["plugins"].as_table().unwrap();
        assert_eq!(plugins.len(), 2);
        assert!(plugins.contains_key("audit"));
        assert!(plugins.contains_key("fmt"));
    }

    #[test]
    fn map_of_fills_defaults_into_each_entry() {
        let dir = TempDir::new().unwrap();
        // Two entries: `audit` omits `enabled`, `fmt` sets it. The default
        // (`false`) should fill `audit.enabled` without touching `fmt.enabled`.
        fs::write(
            dir.path().join("demo.toml"),
            "[plugins.audit]\nseverity = \"warn\"\n[plugins.fmt]\nenabled = true\nseverity = \"e\"\n",
        )
        .unwrap();
        let table = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        let plugins = table["plugins"].as_table().unwrap();
        assert!(
            !plugins["audit"].as_table().unwrap()["enabled"]
                .as_bool()
                .unwrap(),
            "missing leaf in map entry must get the default"
        );
        assert!(
            plugins["fmt"].as_table().unwrap()["enabled"]
                .as_bool()
                .unwrap(),
            "explicit leaf in map entry must not be overwritten"
        );
    }

    #[test]
    fn map_of_required_field_in_entry_errors_when_missing() {
        // `severity` is required (no default) on the item schema. An
        // entry missing it must surface a MissingRequired pointing at the
        // entry-qualified path.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[plugins.audit]\nenabled = true\n",
        )
        .unwrap();
        let result = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();
        match result.unwrap_err() {
            ClapfigError::MissingRequired { key } => {
                assert_eq!(key, "plugins.audit.severity");
            }
            other => panic!("expected MissingRequired, got {other:?}"),
        }
    }

    #[test]
    fn map_of_unknown_key_in_entry_is_flagged_with_entry_path() {
        // Unknown keys inside a map entry: dotted path includes the entry
        // key. `plugins.audit.rogue` is the path the cascade walks.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[plugins.audit]\nseverity = \"warn\"\nrogue = 1\n",
        )
        .unwrap();
        let err = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .load()
            .unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "plugins.audit.rogue");
    }

    #[test]
    fn map_of_empty_is_valid_when_absent() {
        // Like ArrayOf, an absent MapOf is the empty map — not an error.
        let dir = TempDir::new().unwrap();
        let table = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        // `plugins` may or may not be present in the resulting table;
        // what matters is that load doesn't error.
        if let Some(plugins) = table.get("plugins") {
            let plugins_table = plugins.as_table().unwrap();
            assert!(plugins_table.is_empty());
        }
    }

    #[test]
    fn map_of_json_schema_emits_additional_properties() {
        let result = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Schema { output: None })
            .unwrap();
        match result {
            ConfigResult::Schema(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                let plugins = &v["properties"]["plugins"];
                assert_eq!(plugins["type"], "object");
                let additional = &plugins["additionalProperties"];
                assert_eq!(additional["type"], "object");
                assert_eq!(additional["title"], "Plugin");
                // Required-field listing recurses into the per-entry schema.
                let req: Vec<&str> = additional["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap())
                    .collect();
                assert!(req.contains(&"severity"));
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn map_of_invalid_value_shape_errors_on_load() {
        // `[plugins]` is a leaf scalar in the source file. The schema says
        // it must be a table-of-tables; loading must error.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "plugins = \"oops\"\n").unwrap();
        let result = Clapfig::runtime(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();
        match result.unwrap_err() {
            ClapfigError::InvalidValue { key, reason } => {
                assert_eq!(key, "plugins");
                assert!(reason.contains("expected table"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn array_of_keys_not_addressable_via_persist_set() {
        // Regression: `valid_keys` used to recurse into ArrayOf subtrees,
        // making `plugins.id` look like a valid persist target. But the
        // persist path builds nested tables (not arrays-of-tables), so
        // writing `plugins.id` would produce `[plugins] id = "..."` and
        // then runtime validation would reject the result with
        // "expected array, got table". The fix excludes ArrayOf subtrees
        // from `valid_keys`; the user-facing symptom is a clean
        // `KeyNotFound` instead of a corrupted file.
        let dir = TempDir::new().unwrap();
        let schema = Schema::object("Top").array_of(
            "plugins",
            Schema::object("Plugin").field("id", RtField::string()),
        );
        let result = Clapfig::runtime(schema.build())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "plugins.id".into(),
                value: "x".into(),
                scope: None,
            });
        assert!(
            matches!(result, Err(ClapfigError::KeyNotFound(_))),
            "expected KeyNotFound for ArrayOf-internal key, got {result:?}"
        );
        // File must not have been written.
        assert!(!dir.path().join("demo.toml").exists());
    }

    #[test]
    fn handle_schema_emits_enum_array_and_descriptions() {
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Schema { output: None })
            .unwrap();

        match result {
            ConfigResult::Schema(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                let level = &v["properties"]["level"];
                let enum_arr = level["enum"].as_array().expect("enum array");
                assert_eq!(enum_arr.len(), 4);
                assert_eq!(level["description"], "Log verbosity");
                // Nested has its own properties block.
                assert!(v["properties"]["db"]["properties"]["url"].is_object());
            }
            other => panic!("expected Schema, got {other:?}"),
        }
    }

    #[test]
    fn handle_get_merged_returns_value_and_doc() {
        let dir = TempDir::new().unwrap();
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Get {
                key: "port".into(),
                scope: None,
            })
            .unwrap();

        match result {
            ConfigResult::KeyValue { value, doc, .. } => {
                assert_eq!(value, "8080");
                assert!(doc.iter().any(|l| l.contains("Port number")));
            }
            other => panic!("expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn handle_set_persists_to_file() {
        let dir = TempDir::new().unwrap();
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "12345".into(),
                scope: None,
            })
            .unwrap();
        assert!(matches!(result, ConfigResult::ValueSet { .. }));
        let content = fs::read_to_string(dir.path().join("demo.toml")).unwrap();
        assert!(content.contains("port = 12345"));
    }

    #[test]
    fn handle_unset_removes_value() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 12345\nhost = \"x\"\n").unwrap();
        let result = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Unset {
                key: "port".into(),
                scope: None,
            })
            .unwrap();
        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
        let content = fs::read_to_string(dir.path().join("demo.toml")).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"x\""));
    }

    // --- cli_overrides_from auto-matching ---

    #[test]
    fn cli_overrides_from_matches_known_keys_only() {
        #[derive(serde::Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<i64>,
            verbose: bool, // not in schema
        }
        let args = Args {
            host: Some("from-cli".into()),
            port: Some(4242),
            verbose: true,
        };
        let dir = TempDir::new().unwrap();
        let table = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_overrides_from(&args)
            .load()
            .unwrap();
        assert_eq!(table.get("host"), Some(&Value::String("from-cli".into())));
        assert_eq!(table.get("port"), Some(&Value::Integer(4242)));
        // `verbose` was silently ignored — not in schema.
        assert!(table.get("verbose").is_none());
    }

    // --- Phase 3 cascading strictness (#37) ---

    use crate::{UnknownKeyContext, UnknownKeyDecision};

    fn three_level_schema() -> Schema {
        // Top -> mid -> deep, each a nested section. Used for the
        // 3-level-cascade tests.
        Schema::object("Top")
            .field("name", RtField::string().default("x"))
            .nested(
                "mid",
                Schema::object("Mid")
                    .field("m_field", RtField::string().default("mv"))
                    .nested(
                        "deep",
                        Schema::object("Deep").field("d_field", RtField::string().default("dv")),
                    ),
            )
            .build()
    }

    #[test]
    fn schema_strict_cascade_through_three_levels() {
        // Top: strict false (the runtime equivalent of strict_at("", false))
        // mid + deep inherit lenient. Unknown key 4 levels deep drops.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "[mid.deep]\nrogue = 1\n").unwrap();
        let mut schema = three_level_schema();
        schema.strict = Some(false);
        let table = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        // Unknown key dropped silently; the merged table mirrors what was
        // in the file.
        assert!(
            table
                .get("mid")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("deep"))
                .is_some()
        );
    }

    #[test]
    fn descendant_can_re_tighten_subtree() {
        // mid is lenient, mid.deep re-tightens — rogue at mid drops, rogue
        // at mid.deep errors.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[mid]\nm_field = \"v\"\nmid_rogue = 1\n[mid.deep]\nd_field = \"v\"\ndeep_rogue = 1\n",
        )
        .unwrap();
        let schema = Schema::object("Top")
            .field("name", RtField::string().default("x"))
            .nested(
                "mid",
                Schema::object("Mid")
                    .strict(false)
                    .field("m_field", RtField::string().default("v"))
                    .nested(
                        "deep",
                        Schema::object("Deep")
                            .strict(true)
                            .field("d_field", RtField::string().default("v")),
                    ),
            )
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
        assert!(
            names.contains(&"mid.deep.deep_rogue"),
            "deep_rogue should be rejected: {names:?}"
        );
        assert!(
            !names.contains(&"mid.mid_rogue"),
            "mid_rogue should be lenient under strict(false): {names:?}"
        );
    }

    #[test]
    fn runtime_strict_at_overlays_schema_strict() {
        // Schema sets mid strict=false; builder strict_at("mid", true)
        // overrides. Result: mid rogue is rejected.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[mid]\nm_field = \"v\"\nrogue = 1\n",
        )
        .unwrap();
        let schema = Schema::object("Top")
            .field("name", RtField::string().default("x"))
            .nested(
                "mid",
                Schema::object("Mid")
                    .strict(false)
                    .field("m_field", RtField::string().default("v")),
            )
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict_at("mid", true) // overlay re-tightens
            .load();
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key, "mid.rogue");
    }

    #[test]
    fn runtime_lex_fmt_style_sibling_callback() {
        // The use-case from the proposal: typed fields and a free-form
        // catch-all share a struct level. The cascade alone can't
        // distinguish them; the callback applies a domain-specific rule
        // (here: "leaf contains a `.` → accept, else reject").
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[diagnostics.rules]\nmissing_footote = \"warn\"\n\"acme.task-due-date-missing\" = \"error\"\n",
        )
        .unwrap();
        let schema = Schema::object("Cfg")
            .nested(
                "diagnostics",
                Schema::object("Diag").nested("rules", Schema::object("Rules")), // empty rules: any key is unknown
            )
            .build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .on_unknown_key(|c: &UnknownKeyContext<'_>| {
                if c.path.starts_with("diagnostics.rules.") && c.leaf.contains('.') {
                    UnknownKeyDecision::Accept
                } else {
                    UnknownKeyDecision::Reject
                }
            })
            .load();
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("expected UnknownKeys");
        let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
        assert!(
            names.iter().any(|k| k.contains("missing_footote")),
            "bare typo must be rejected: {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|k| k.contains("acme.task-due-date-missing")),
            "dotted extension key must be accepted: {names:?}"
        );
    }

    // --- `accept_dotted_extension_keys_in` helper (issue #54 item 6) ---

    fn dotted_ext_schema() -> Schema {
        Schema::object("Cfg")
            .nested(
                "diagnostics",
                Schema::object("Diag").nested("rules", Schema::object("Rules")),
            )
            .build()
    }

    #[test]
    fn dotted_ext_helper_accepts_dotted_under_path() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[diagnostics.rules]\n\"acme.task-due-date-missing\" = \"error\"\n",
        )
        .unwrap();
        let result = Clapfig::runtime(dotted_ext_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .accept_dotted_extension_keys_in("diagnostics.rules", crate::UnknownKeyDecision::Accept)
            .load();
        assert!(
            result.is_ok(),
            "dotted leaf under configured path must be accepted: {:?}",
            result.err()
        );
    }

    #[test]
    fn dotted_ext_helper_rejects_bare_typo_under_path() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[diagnostics.rules]\nmissing_footote = \"warn\"\n",
        )
        .unwrap();
        let result = Clapfig::runtime(dotted_ext_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .accept_dotted_extension_keys_in("diagnostics.rules", crate::UnknownKeyDecision::Accept)
            .load();
        let err = result.unwrap_err();
        let keys = err.unknown_keys().expect("UnknownKeys");
        assert_eq!(keys.len(), 1);
        assert!(keys[0].key.contains("missing_footote"));
    }

    #[test]
    fn dotted_ext_helper_path_boundary_enforced_by_segment() {
        // `diag` would substring-match `diagnostics` but the helper
        // enforces a segment boundary, so the rule does NOT apply.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[diagnostics.rules]\n\"acme.x\" = \"warn\"\n",
        )
        .unwrap();
        let result = Clapfig::runtime(dotted_ext_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .accept_dotted_extension_keys_in("diag", crate::UnknownKeyDecision::Accept)
            .load();
        // `diag` is not a real prefix of `diagnostics.rules` at segment
        // level, so the helper's rule doesn't fire — the dotted key
        // falls through to Reject.
        assert!(result.is_err());
    }

    #[test]
    fn dotted_ext_helper_collect_routes_into_load_with_unknowns_list() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[diagnostics.rules]\n\"acme.x-rule\" = \"warn\"\n",
        )
        .unwrap();
        let (_table, unknowns) = Clapfig::runtime(dotted_ext_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .accept_dotted_extension_keys_in(
                "diagnostics.rules",
                crate::UnknownKeyDecision::Collect,
            )
            .load_with_unknowns()
            .unwrap();
        assert_eq!(unknowns.len(), 1);
        assert_eq!(unknowns[0].leaf, "acme.x-rule");
    }

    #[test]
    fn dotted_ext_helper_empty_path_applies_everywhere() {
        // Empty path → rule applies at every level. A dotted leaf at the
        // top level (rare but possible with quoted keys) is accepted.
        let dir = TempDir::new().unwrap();
        // Use a schema with NO declared sections so the top-level dotted
        // key is unknown to the schema. Using an empty Cfg schema with
        // a known field would still leave the dotted key unknown — what
        // matters is the unknown-key path triggers.
        fs::write(dir.path().join("demo.toml"), "\"acme.x\" = \"warn\"\n").unwrap();
        let schema = Schema::object("Cfg").build();
        let result = Clapfig::runtime(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .accept_dotted_extension_keys_in("", crate::UnknownKeyDecision::Accept)
            .load();
        assert!(
            result.is_ok(),
            "empty path must apply rule globally: {:?}",
            result.err()
        );
    }

    // --- RuntimeResolver cache behavior (parity with Resolver<C> tests) ---

    fn resolver_with_path(dir: &std::path::Path) -> RuntimeResolver {
        Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.to_path_buf())])
            .no_env()
            .build_resolver()
            .unwrap()
    }

    #[test]
    fn cache_populates_on_first_read() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 3000\n").unwrap();
        let resolver = resolver_with_path(dir.path());
        assert_eq!(resolver.cache_size(), 0);
        resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(resolver.cache_size(), 1);
    }

    #[test]
    fn cache_hit_on_second_read_of_same_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("demo.toml");
        fs::write(&path, "port = 3000\n").unwrap();
        let resolver = resolver_with_path(dir.path());

        let table1 = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(table1.get("port"), Some(&Value::Integer(3000)));
        assert_eq!(resolver.cache_size(), 1);

        // Rewrite the file on disk. If the cache is honored, the second
        // resolve returns the ORIGINAL value, not the new one — the
        // contract is "no mtime check; build a new resolver for
        // freshness," same as the static Resolver<C>.
        fs::write(&path, "port = 9999\n").unwrap();
        let table2 = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(
            table2.get("port"),
            Some(&Value::Integer(3000)),
            "cache should mask on-disk changes"
        );
        assert_eq!(resolver.cache_size(), 1, "no new cache entry");
    }

    #[test]
    fn cache_shared_ancestor_across_resolves_dedups() {
        use crate::types::Boundary;
        let root = TempDir::new().unwrap();
        let a_leaf = root.path().join("a");
        let b_leaf = root.path().join("b");
        fs::create_dir_all(&a_leaf).unwrap();
        fs::create_dir_all(&b_leaf).unwrap();
        // Only the shared root file exists.
        fs::write(root.path().join("demo.toml"), "port = 7777\n").unwrap();

        let resolver = Clapfig::runtime(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Ancestors(Boundary::Root)])
            .no_env()
            .build_resolver()
            .unwrap();

        resolver.resolve_at(&a_leaf).unwrap();
        let cache_after_a = resolver.cache_size();
        resolver.resolve_at(&b_leaf).unwrap();
        let cache_after_b = resolver.cache_size();

        assert!(cache_after_a >= 1);
        assert_eq!(
            cache_after_b, cache_after_a,
            "shared ancestor file should be deduplicated in cache"
        );
    }
}
