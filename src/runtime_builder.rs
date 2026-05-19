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
}

impl RuntimeBuilder {
    pub(crate) fn new(schema: Schema) -> Self {
        Self {
            spec: Arc::new(DynamicSpec::new(schema)),
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
        }
    }

    pub fn app_name(mut self, name: &str) -> Self {
        self.app_name = Some(name.to_string());
        self
    }

    pub fn file_name(mut self, name: &str) -> Self {
        self.file_name = Some(name.to_string());
        self
    }

    pub fn search_paths(mut self, paths: Vec<SearchPath>) -> Self {
        self.search_paths = Some(paths);
        self
    }

    pub fn add_search_path(mut self, path: SearchPath) -> Self {
        self.search_paths
            .get_or_insert_with(|| vec![SearchPath::Platform])
            .push(path);
        self
    }

    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = mode;
        self
    }

    pub fn persist_scope(mut self, name: &str, path: SearchPath) -> Self {
        self.persist_scopes.push((name.to_string(), path));
        self
    }

    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    pub fn no_env(mut self) -> Self {
        self.env_enabled = false;
        self
    }

    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    pub fn normalize_keys(mut self, normalize: bool) -> Self {
        self.normalize_keys = normalize;
        self
    }

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

    #[cfg(feature = "url")]
    pub fn url_query(mut self, query: &str) -> Self {
        self.url_overrides
            .extend(crate::url::query_to_overrides(query));
        self
    }

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

        Ok(RuntimeResolver {
            spec: self.spec,
            app_name,
            file_name,
            search_paths,
            search_mode: self.search_mode,
            env_prefix,
            env_vars,
            strict: self.strict,
            normalize_keys: self.normalize_keys,
            #[cfg(feature = "url")]
            url_overrides: self.url_overrides,
            cli_overrides: self.cli_overrides,
            layer_order: self.layer_order,
            post_validate: self.post_validate.map(Arc::new),
            file_cache: Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn load(self) -> Result<Table, ClapfigError> {
        let start_dir = std::env::current_dir().map_err(|e| ClapfigError::IoError {
            path: PathBuf::from("."),
            source: e,
        })?;
        self.build_resolver()?.resolve_at(start_dir)
    }

    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError> {
        let result = self.handle(action)?;
        print!("{result}");
        Ok(())
    }

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
    strict: bool,
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
            strict: self.strict,
            normalize_keys: self.normalize_keys,
            layer_order: self.layer_order.clone(),
        };

        let table = resolve::resolve(input)?;
        if let Some(hook) = self.post_validate.as_ref() {
            hook(&table).map_err(ClapfigError::PostValidationFailed)?;
        }
        Ok(table)
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
}
