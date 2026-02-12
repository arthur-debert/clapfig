use std::marker::PhantomData;

use confique::Config;
use serde::{Deserialize, Serialize};

use crate::error::ClapfigError;
use crate::file;
use crate::flatten;
use crate::ops::{self, ConfigResult};
use crate::overrides;
use crate::persist;
use crate::resolve::{self, ResolveInput};
use crate::types::{ConfigAction, SearchMode, SearchPath};

/// Entry point for building a clapfig configuration.
pub struct Clapfig;

impl Clapfig {
    pub fn builder<C: Config>() -> ClapfigBuilder<C> {
        ClapfigBuilder::new()
    }
}

/// Builder for configuring and loading layered configuration.
///
/// Controls three orthogonal axes (see [`types`](crate::types) for the full picture):
///
/// - **Discovery**: [`search_paths()`](Self::search_paths) — where to look for config files.
/// - **Resolution**: [`search_mode()`](Self::search_mode) — merge all or pick one.
/// - **Persistence**: [`persist_path()`](Self::persist_path) — where `config set` writes.
pub struct ClapfigBuilder<C: Config> {
    app_name: Option<String>,
    file_name: Option<String>,
    search_paths: Option<Vec<SearchPath>>,
    search_mode: SearchMode,
    persist_path: Option<SearchPath>,
    env_prefix: Option<String>,
    env_enabled: bool,
    strict: bool,
    cli_overrides: Vec<(String, toml::Value)>,
    _phantom: PhantomData<C>,
}

impl<C: Config> ClapfigBuilder<C> {
    fn new() -> Self {
        Self {
            app_name: None,
            file_name: None,
            search_paths: None,
            search_mode: SearchMode::default(),
            persist_path: None,
            env_prefix: None,
            env_enabled: true,
            strict: true,
            cli_overrides: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Set the application name. This derives sensible defaults:
    /// - `file_name` → `"{app_name}.toml"`
    /// - `search_paths` → `[SearchPath::Platform]`
    /// - `env_prefix` → `"{APP_NAME}"` (uppercased)
    pub fn app_name(mut self, name: &str) -> Self {
        self.app_name = Some(name.to_string());
        self
    }

    /// Override the config file name (default: `"{app_name}.toml"`).
    pub fn file_name(mut self, name: &str) -> Self {
        self.file_name = Some(name.to_string());
        self
    }

    /// Replace the default search paths entirely.
    ///
    /// Paths are listed in **priority-ascending** order: the last entry has the
    /// highest priority. See [`SearchPath`] for the available variants.
    pub fn search_paths(mut self, paths: Vec<SearchPath>) -> Self {
        self.search_paths = Some(paths);
        self
    }

    /// Append a search path without replacing the defaults.
    /// If no paths have been set yet, starts from the default `[Platform]`.
    pub fn add_search_path(mut self, path: SearchPath) -> Self {
        self.search_paths
            .get_or_insert_with(|| vec![SearchPath::Platform])
            .push(path);
        self
    }

    /// Set the search mode (default: [`SearchMode::Merge`]).
    ///
    /// - [`Merge`](SearchMode::Merge): all found config files are deep-merged,
    ///   later (higher-priority) files overriding earlier ones.
    /// - [`FirstMatch`](SearchMode::FirstMatch): only the single highest-priority
    ///   config file found is used.
    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = mode;
        self
    }

    /// Set the persistence path for `config set`.
    ///
    /// This is where `config set` writes values. It is independent of the search
    /// paths used for reading. Must be a single-directory variant (`Platform`,
    /// `Home`, `Cwd`, or `Path`). Using [`Ancestors`](SearchPath::Ancestors)
    /// produces an error at build time.
    ///
    /// If not set, `config set` returns [`ClapfigError::NoPersistPath`].
    pub fn persist_path(mut self, path: SearchPath) -> Self {
        self.persist_path = Some(path);
        self
    }

    /// Override the environment variable prefix (default: uppercased `app_name`).
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Disable environment variable loading entirely.
    pub fn no_env(mut self) -> Self {
        self.env_enabled = false;
        self
    }

    /// Enable or disable strict mode (default: `true`).
    /// In strict mode, unknown keys in config files produce errors.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Add a CLI override. `None` values are ignored (useful for optional clap args).
    pub fn cli_override<V: Into<toml::Value>>(mut self, key: &str, value: Option<V>) -> Self {
        if let Some(v) = value {
            self.cli_overrides.push((key.to_string(), v.into()));
        }
        self
    }

    /// Add CLI overrides from any serializable source, auto-matching by field name.
    ///
    /// Serializes `source` into flat key-value pairs, skips `None` values, and keeps
    /// only keys that match config fields in `C`. Non-matching keys are silently ignored,
    /// so clap-only fields like `command` or `verbose` are automatically excluded.
    ///
    /// Works with clap-derived structs, `HashMap`s, or anything implementing `Serialize`.
    ///
    /// Composes with [`cli_override`](Self::cli_override) — both push to the same
    /// override list. Later calls take precedence.
    pub fn cli_overrides_from<S: Serialize>(mut self, source: &S) -> Self {
        let pairs = flatten::flatten(source)
            .expect("clapfig: failed to flatten CLI source for auto-matching");
        let valid = overrides::valid_keys(&C::META);
        for (key, value) in pairs {
            if let Some(v) = value
                && valid.contains(&key)
            {
                self.cli_overrides.push((key, v));
            }
        }
        self
    }

    /// Resolve the effective app name, or error if not set.
    fn effective_app_name(&self) -> Result<&str, ClapfigError> {
        self.app_name
            .as_deref()
            .ok_or(ClapfigError::AppNameRequired)
    }

    /// Resolve the effective file name.
    fn effective_file_name(&self) -> Result<String, ClapfigError> {
        if let Some(name) = &self.file_name {
            return Ok(name.clone());
        }
        let app = self.effective_app_name()?;
        Ok(format!("{app}.toml"))
    }

    /// Resolve the effective search paths.
    fn effective_search_paths(&self) -> Vec<SearchPath> {
        if let Some(paths) = &self.search_paths {
            return paths.clone();
        }
        vec![SearchPath::Platform]
    }

    /// Resolve the effective env prefix (None if env disabled).
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

    /// Build the `ResolveInput` from current builder state.
    fn build_input(&self) -> Result<ResolveInput, ClapfigError>
    where
        C::Layer: for<'de> Deserialize<'de>,
    {
        let app_name = self.effective_app_name()?;
        let file_name = self.effective_file_name()?;
        let search_paths = self.effective_search_paths();
        let env_prefix = self.effective_env_prefix()?;

        let files = file::load_config_files(&search_paths, &file_name, app_name, self.search_mode)?;
        let env_vars: Vec<(String, String)> = std::env::vars().collect();

        Ok(ResolveInput {
            files,
            env_vars,
            env_prefix,
            cli_overrides: self.cli_overrides.clone(),
            strict: self.strict,
        })
    }

    /// Load and resolve the configuration through all layers.
    pub fn load(self) -> Result<C, ClapfigError>
    where
        C::Layer: for<'de> Deserialize<'de>,
    {
        let input = self.build_input()?;
        resolve::resolve(input)
    }

    /// Handle a `ConfigAction` and print the result to stdout.
    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError>
    where
        C: Serialize,
        C::Layer: for<'de> Deserialize<'de>,
    {
        let result = self.handle(action)?;
        print!("{result}");
        Ok(())
    }

    /// Handle a `ConfigAction` (list / gen / get / set / unset).
    pub fn handle(self, action: &ConfigAction) -> Result<ConfigResult, ClapfigError>
    where
        C: Serialize,
        C::Layer: for<'de> Deserialize<'de>,
    {
        match action {
            ConfigAction::List => {
                let config = self.load()?;
                ops::list_values(&config)
            }
            ConfigAction::Gen { output } => {
                let template = ops::generate_template::<C>();
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
            ConfigAction::Get { key } => {
                let config = self.load()?;
                ops::get_value(&config, key)
            }
            ConfigAction::Set { key, value } => {
                let app_name = self.effective_app_name()?;
                let file_name = self.effective_file_name()?;
                let persist = self
                    .persist_path
                    .as_ref()
                    .ok_or(ClapfigError::NoPersistPath)?;

                let path = file::resolve_persist_path(persist, &file_name, app_name)?;

                persist::persist_value::<C>(&path, key, value)
            }
            ConfigAction::Unset { key } => {
                let app_name = self.effective_app_name()?;
                let file_name = self.effective_file_name()?;
                let persist = self
                    .persist_path
                    .as_ref()
                    .ok_or(ClapfigError::NoPersistPath)?;

                let path = file::resolve_persist_path(persist, &file_name, app_name)?;

                persist::unset_value(&path, key)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::test::TestConfig;
    use crate::types::Boundary;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn app_name_sets_defaults() {
        let builder = Clapfig::builder::<TestConfig>().app_name("myapp");
        assert_eq!(builder.effective_file_name().unwrap(), "myapp.toml");
        assert_eq!(
            builder.effective_env_prefix().unwrap(),
            Some("MYAPP".to_string())
        );
        assert_eq!(builder.effective_search_paths(), vec![SearchPath::Platform]);
    }

    #[test]
    fn override_file_name() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .file_name("custom.toml");
        assert_eq!(builder.effective_file_name().unwrap(), "custom.toml");
    }

    #[test]
    fn override_env_prefix() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .env_prefix("CUSTOM");
        assert_eq!(
            builder.effective_env_prefix().unwrap(),
            Some("CUSTOM".to_string())
        );
    }

    #[test]
    fn no_env_disables_prefix() {
        let builder = Clapfig::builder::<TestConfig>().app_name("myapp").no_env();
        assert_eq!(builder.effective_env_prefix().unwrap(), None);
    }

    #[test]
    fn search_paths_replace() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .search_paths(vec![SearchPath::Cwd]);
        assert_eq!(builder.effective_search_paths(), vec![SearchPath::Cwd]);
    }

    #[test]
    fn add_search_path_appends_to_defaults() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .add_search_path(SearchPath::Cwd);
        assert_eq!(
            builder.effective_search_paths(),
            vec![SearchPath::Platform, SearchPath::Cwd]
        );
    }

    #[test]
    fn add_search_path_appends_to_existing_list() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .search_paths(vec![SearchPath::Cwd])
            .add_search_path(SearchPath::Platform);
        assert_eq!(
            builder.effective_search_paths(),
            vec![SearchPath::Cwd, SearchPath::Platform]
        );
    }

    #[test]
    fn search_mode_defaults_to_merge() {
        let builder = Clapfig::builder::<TestConfig>().app_name("myapp");
        assert_eq!(builder.search_mode, SearchMode::Merge);
    }

    #[test]
    fn search_mode_can_be_set() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .search_mode(SearchMode::FirstMatch);
        assert_eq!(builder.search_mode, SearchMode::FirstMatch);
    }

    #[test]
    fn persist_path_defaults_to_none() {
        let builder = Clapfig::builder::<TestConfig>().app_name("myapp");
        assert!(builder.persist_path.is_none());
    }

    #[test]
    fn persist_path_can_be_set() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .persist_path(SearchPath::Platform);
        assert_eq!(builder.persist_path, Some(SearchPath::Platform));
    }

    #[test]
    fn cli_override_some_added() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .cli_override("port", Some(3000i64));
        assert_eq!(builder.cli_overrides.len(), 1);
        assert_eq!(builder.cli_overrides[0].0, "port");
    }

    #[test]
    fn cli_override_none_skipped() {
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("myapp")
            .cli_override::<i64>("port", None);
        assert!(builder.cli_overrides.is_empty());
    }

    #[test]
    fn missing_app_name_errors() {
        let builder = Clapfig::builder::<TestConfig>();
        let result = builder.load();
        assert!(matches!(result, Err(ClapfigError::AppNameRequired)));
    }

    // --- Load tests ---

    #[test]
    fn load_with_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(config.port, 3000);
        assert_eq!(config.host, "localhost"); // default preserved
    }

    #[test]
    fn load_with_cli_override() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_override("port", Some(9999i64))
            .load()
            .unwrap();

        assert_eq!(config.port, 9999);
    }

    #[test]
    fn load_defaults_only() {
        let dir = TempDir::new().unwrap();
        // No config file — just defaults
        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 8080);
        assert!(!config.debug);
    }

    #[test]
    fn strict_rejects_unknown_key() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "typo = 1\n").unwrap();

        let result: Result<TestConfig, _> = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .load();

        assert!(result.is_err());
    }

    #[test]
    fn lenient_allows_unknown_key() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "typo = 1\nport = 3000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(false)
            .load()
            .unwrap();

        assert_eq!(config.port, 3000);
    }

    // --- SearchMode tests ---

    #[test]
    fn first_match_uses_highest_priority_file_only() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(
            dir1.path().join("test.toml"),
            "port = 1000\nhost = \"low\"\n",
        )
        .unwrap();
        fs::write(dir2.path().join("test.toml"), "port = 2000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()), // highest priority
            ])
            .search_mode(SearchMode::FirstMatch)
            .no_env()
            .load()
            .unwrap();

        // Should use dir2 only — port from dir2, host from defaults (not dir1!)
        assert_eq!(config.port, 2000);
        assert_eq!(config.host, "localhost"); // default, NOT "low" from dir1
    }

    #[test]
    fn merge_mode_combines_both_files() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(
            dir1.path().join("test.toml"),
            "port = 1000\nhost = \"base\"\n",
        )
        .unwrap();
        fs::write(dir2.path().join("test.toml"), "port = 2000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()),
            ])
            .search_mode(SearchMode::Merge)
            .no_env()
            .load()
            .unwrap();

        // Merge: port from dir2 (higher priority), host from dir1 (lower priority)
        assert_eq!(config.port, 2000);
        assert_eq!(config.host, "base");
    }

    #[test]
    fn first_match_falls_back_when_high_priority_missing() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        // Only dir1 (lower priority) has a config
        fs::write(dir1.path().join("test.toml"), "port = 1000\n").unwrap();

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()),
            ])
            .search_mode(SearchMode::FirstMatch)
            .no_env()
            .load()
            .unwrap();

        assert_eq!(config.port, 1000);
    }

    // --- handle tests ---

    #[test]
    fn handle_gen() {
        let result: ConfigResult = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .no_env()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();

        match result {
            ConfigResult::Template(t) => {
                assert!(t.contains("host"));
                assert!(t.contains("port"));
            }
            other => panic!("Expected Template, got {other:?}"),
        }
    }

    #[test]
    fn handle_gen_with_output() {
        let dir = TempDir::new().unwrap();
        let out_path = dir.path().join("generated.toml");

        let result: ConfigResult = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .no_env()
            .handle(&ConfigAction::Gen {
                output: Some(out_path.clone()),
            })
            .unwrap();

        assert!(matches!(result, ConfigResult::TemplateWritten { .. }));
        let content = fs::read_to_string(&out_path).unwrap();
        assert!(content.contains("host"));
        assert!(content.contains("port"));
    }

    #[test]
    fn handle_get() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Get { key: "port".into() })
            .unwrap();

        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "3000"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn handle_set_requires_persist_path() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
            });

        assert!(matches!(result, Err(ClapfigError::NoPersistPath)));
    }

    #[test]
    fn handle_set_with_persist_path() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .persist_path(SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
            })
            .unwrap();

        assert!(matches!(result, ConfigResult::ValueSet { .. }));
        let content = fs::read_to_string(dir.path().join("test.toml")).unwrap();
        assert!(content.contains("port = 3000"));
    }

    #[test]
    fn handle_unset_requires_persist_path() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Unset { key: "port".into() });

        assert!(matches!(result, Err(ClapfigError::NoPersistPath)));
    }

    #[test]
    fn handle_unset_removes_key() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("test.toml"),
            "port = 3000\nhost = \"localhost\"\n",
        )
        .unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .persist_path(SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Unset { key: "port".into() })
            .unwrap();

        assert!(matches!(result, ConfigResult::ValueUnset { .. }));
        let content = fs::read_to_string(dir.path().join("test.toml")).unwrap();
        assert!(!content.contains("port"));
        assert!(content.contains("host = \"localhost\""));
    }

    #[test]
    fn handle_set_rejects_ancestors_persist_path() {
        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .persist_path(SearchPath::Ancestors(Boundary::Root))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
            });

        assert!(matches!(
            result,
            Err(ClapfigError::AncestorsNotAllowedAsPersistPath)
        ));
    }

    #[test]
    fn handle_set_persist_path_independent_of_search_paths() {
        let search_dir = TempDir::new().unwrap();
        let persist_dir = TempDir::new().unwrap();

        fs::write(search_dir.path().join("test.toml"), "port = 1000\n").unwrap();

        // persist_path points somewhere different from search_paths
        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(search_dir.path().to_path_buf())])
            .persist_path(SearchPath::Path(persist_dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "5000".into(),
            })
            .unwrap();

        assert!(matches!(result, ConfigResult::ValueSet { .. }));
        // Written to persist_dir, not search_dir
        let content = fs::read_to_string(persist_dir.path().join("test.toml")).unwrap();
        assert!(content.contains("port = 5000"));
        // search_dir file unchanged
        let original = fs::read_to_string(search_dir.path().join("test.toml")).unwrap();
        assert!(original.contains("port = 1000"));
    }

    // --- cli_overrides_from tests ---

    #[test]
    fn overrides_from_matches_known_keys() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<u16>,
        }
        let args = Args {
            host: Some("1.2.3.4".into()),
            port: Some(9999),
        };
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_overrides_from(&args);
        assert_eq!(builder.cli_overrides.len(), 2);
    }

    #[test]
    fn overrides_from_skips_none() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<u16>,
        }
        let args = Args {
            host: None,
            port: Some(9999),
        };
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_overrides_from(&args);
        assert_eq!(builder.cli_overrides.len(), 1);
        assert_eq!(builder.cli_overrides[0].0, "port");
    }

    #[test]
    fn overrides_from_ignores_unknown_keys() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            verbose: bool,
            output: Option<String>,
        }
        let args = Args {
            host: Some("x".into()),
            verbose: true,
            output: Some("f".into()),
        };
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_overrides_from(&args);
        assert_eq!(builder.cli_overrides.len(), 1);
        assert_eq!(builder.cli_overrides[0].0, "host");
    }

    #[test]
    fn overrides_from_composes_with_cli_override() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
        }
        let args = Args {
            host: Some("from_struct".into()),
        };
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_override("port", Some(1234i64))
            .cli_overrides_from(&args);
        assert_eq!(builder.cli_overrides.len(), 2);
        assert_eq!(builder.cli_overrides[0].0, "port");
        assert_eq!(builder.cli_overrides[1].0, "host");
    }

    #[test]
    fn overrides_from_hashmap() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("port".to_string(), 3000i64);
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_overrides_from(&map);
        assert_eq!(builder.cli_overrides.len(), 1);
        assert_eq!(builder.cli_overrides[0].0, "port");
    }

    #[test]
    fn overrides_from_all_none() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<u16>,
        }
        let args = Args {
            host: None,
            port: None,
        };
        let builder = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .cli_overrides_from(&args);
        assert!(builder.cli_overrides.is_empty());
    }

    #[test]
    fn overrides_from_end_to_end() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<i64>,
            verbose: bool,
        }
        let args = Args {
            host: Some("1.2.3.4".into()),
            port: None,
            verbose: true,
        };

        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_overrides_from(&args)
            .load()
            .unwrap();

        assert_eq!(config.host, "1.2.3.4"); // from cli
        assert_eq!(config.port, 3000); // from file (cli was None)
        assert!(!config.debug); // default (verbose not in config)
    }

    #[test]
    fn handle_list() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::List)
            .unwrap();

        match result {
            ConfigResult::Listing { entries } => {
                let port = entries.iter().find(|(k, _)| k == "port").unwrap();
                assert_eq!(port.1, "3000");
                let host = entries.iter().find(|(k, _)| k == "host").unwrap();
                assert_eq!(host.1, "localhost"); // default
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn handle_list_defaults_only() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::List)
            .unwrap();

        match result {
            ConfigResult::Listing { entries } => {
                assert_eq!(entries.len(), 5);
                let db_url = entries.iter().find(|(k, _)| k == "database.url").unwrap();
                assert_eq!(db_url.1, "<not set>");
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }
}
