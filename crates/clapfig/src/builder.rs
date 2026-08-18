//! Builder API for configuring and loading layered configuration.
//!
//! Entry point: [`crate::Clapfig::builder(schema)`](crate::Clapfig::builder).
//! The builder follows a "set what you need, load" pattern: `app_name`
//! derives sensible defaults (file name, search paths, env prefix), and
//! everything else is optional overrides — discovery, persistence, env,
//! URL, CLI overrides, post-validation, tree-walk resolution. `load()`
//! produces a value [`Map`]; the typed
//! [`TypedBuilder`](crate::TypedBuilder) wraps this builder
//! and deserializes that map into a `C` deriving
//! `#[derive(clapfig::Schema)]`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::error::ClapfigError;
use crate::file;
use crate::flatten;
use crate::format::{self, FormatAdapter, FormatRegistry};
use crate::ops::{self, ConfigResult};
use crate::overrides;
use crate::persist;
use crate::resolve::{self, ResolveInput};
use crate::runtime::Schema;
use crate::strict::{StrictnessOverrides, UnknownKeyHook};
use crate::types::{ConfigAction, Layer, SearchMode, SearchPath};
use crate::value::{Map, Value};

/// Post-merge validation hook for the Map-out path: receives the merged
/// value [`Map`]. (The typed path wraps its `Fn(&C)` hook into this shape
/// by deserializing the map first.)
/// Internal post-validate hook shape. Returns a full [`ClapfigError`] so
/// wrappers can distinguish the hook's own rejection (a
/// [`ClapfigError::PostValidationFailed`] carrying the user's message)
/// from failures *around* the user's closure — the typed path's
/// `Map → C` deserialize step categorizes its failure as the
/// [`ClapfigError::InvalidValue`] type error it is.
pub(crate) type PostValidateHook = Box<dyn Fn(&Map) -> Result<(), ClapfigError> + Send + Sync>;

/// How config files are discovered inside each search directory — the
/// file-name half of the builder's file contract.
#[derive(Debug, Clone)]
enum FileNaming {
    /// `.file_name("myapp.toml")` (or the `app_name`-derived default):
    /// exact-name discovery, that name's format only.
    Exact(String),
    /// `.file_stem("myapp")`: probe `<stem>.<ext>` across the enabled
    /// formats' extensions; more than one match in the same directory is
    /// a hard error.
    Stem(String),
}

/// Builder for runtime-defined configurations.
///
/// Controls three orthogonal axes (see [`types`](crate::types) for the full
/// picture):
///
/// - **Discovery**: [`search_paths()`](Self::search_paths) — where to look
///   for config files.
/// - **Resolution**: [`search_mode()`](Self::search_mode) — merge all or
///   pick one.
/// - **Persistence**: [`persist_scope()`](Self::persist_scope) — named
///   targets for writes.
///
/// The schema is supplied as a value (via [`crate::Clapfig::builder`]) and
/// the loaded output is a value [`Map`]. For typed output, derive
/// [`Schema`](crate::Schema) and use
/// [`Clapfig::typed`](crate::Clapfig::typed), whose
/// [`TypedBuilder`](crate::TypedBuilder) forwards to this
/// builder.
pub struct Builder {
    schema: Arc<Schema>,
    app_name: Option<String>,
    file_naming: Option<FileNaming>,
    formats: Option<Vec<String>>,
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
    post_validate: Option<PostValidateHook>,
    strict_at_overrides: Vec<(String, bool)>,
    unknown_key_hook: Option<UnknownKeyHook>,
}

impl Builder {
    pub(crate) fn new(schema: Schema) -> Self {
        Self::from_arc(Arc::new(schema))
    }

    /// Construct a builder reusing an already-`Arc<Schema>`-cached schema
    /// (e.g. the per-type cache the `clapfig::Schema` derive maintains).
    /// Skips the per-builder allocation of the schema tree that
    /// [`new`](Self::new) performs.
    pub(crate) fn from_arc(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            app_name: None,
            file_naming: None,
            formats: None,
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

    /// Set the application name. This derives sensible defaults:
    /// - `file_name` → `"{app_name}.toml"`
    /// - `search_paths` → `[SearchPath::Platform]`
    /// - `env_prefix` → `"{APP_NAME}"` (uppercased)
    pub fn app_name(mut self, name: &str) -> Self {
        self.app_name = Some(name.to_string());
        self
    }

    /// Override the config file name (default: `"{app_name}.toml"`).
    ///
    /// Exact-name discovery: only files with this precise name are
    /// considered, and the name's extension selects the (only) enabled
    /// format. An extensionless name (e.g. an rc file) parses as TOML;
    /// an extension no adapter claims errors at `build_resolver`/`load`
    /// time with [`ClapfigError::UnknownFormat`]. Mutually exclusive with
    /// [`file_stem`](Self::file_stem) — the last call wins.
    pub fn file_name(mut self, name: &str) -> Self {
        self.file_naming = Some(FileNaming::Exact(name.to_string()));
        self
    }

    /// Discover config files by stem across the enabled formats'
    /// extensions (see [`formats`](Self::formats)): `.file_stem("myapp")`
    /// probes `myapp.toml` (and `myapp.yaml` / `myapp.json` when those
    /// formats are enabled) in every search directory.
    ///
    /// More than one same-stem match **in the same directory** is a hard
    /// error naming both files
    /// ([`ClapfigError::AmbiguousConfigFiles`]); across directories,
    /// normal layering applies. Mutually exclusive with
    /// [`file_name`](Self::file_name) — the last call wins.
    pub fn file_stem(mut self, stem: &str) -> Self {
        self.file_naming = Some(FileNaming::Stem(stem.to_string()));
        self
    }

    /// Set the enabled formats for stem-based discovery, in preference
    /// order (canonical names: `"toml"`, `"yaml"`, `"json"`).
    ///
    /// Formats are opt-in and ordered — never inferred from compiled-in
    /// cargo features; the default with no call is TOML only. The first
    /// entry is the app's **preferred format**: `config gen` with no
    /// output path renders it, and `config set` against a scope with no
    /// existing file creates `<stem>.<preferred extension>`. An unknown
    /// name errors at `build_resolver`/`load` time with
    /// [`ClapfigError::UnknownFormat`]; an empty list or a repeated name
    /// errors there with [`ClapfigError::InvalidFormats`].
    pub fn formats<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.formats = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Replace the default search paths entirely.
    ///
    /// Paths are listed in **priority-ascending** order: the last entry has
    /// the highest priority. See [`SearchPath`] for the available variants.
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
    /// - [`Merge`](SearchMode::Merge): all found config files are
    ///   deep-merged, later (higher-priority) files overriding earlier ones.
    /// - [`FirstMatch`](SearchMode::FirstMatch): only the single
    ///   highest-priority config file found is used.
    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = mode;
        self
    }

    /// Add a named persist scope.
    ///
    /// Scopes are named config file targets for `config set`/`unset` (and
    /// optionally `config get`/`list` with `--scope`). The first scope added
    /// is the default for write operations when no `--scope` is specified.
    ///
    /// Scope paths are automatically added to the search paths (if not
    /// already present) so that persisted values are discoverable in the
    /// merged view.
    ///
    /// Must be a single-directory variant (`Platform`, `Home`, `Cwd`, or
    /// `Path`). Using [`Ancestors`](SearchPath::Ancestors) produces an error
    /// at handle time.
    ///
    /// If no scopes are configured, `config set` returns
    /// [`ClapfigError::NoPersistPath`].
    pub fn persist_scope(mut self, name: &str, path: SearchPath) -> Self {
        self.persist_scopes.push((name.to_string(), path));
        self
    }

    /// Override the environment variable prefix (default: uppercased
    /// `app_name`).
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
    ///
    /// This is the **whole-resolution default** in the strictness cascade —
    /// it applies to any unknown key whose ancestors don't carry an explicit
    /// [`strict_at`](Self::strict_at) override or a per-node
    /// [`Schema::strict`](crate::runtime::Schema::strict) setting. See the
    /// [cascading strictness section](crate#cascading-strictness) of the
    /// crate docs for how this composes with `strict_at` and
    /// `on_unknown_key`.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Set per-section strictness for the dotted path `path`.
    ///
    /// Path-aware variant of [`strict`](Self::strict). The override applies
    /// to every descendant of `path` that doesn't itself set strictness —
    /// the same cascade rule used for per-node
    /// [`Schema::strict`](crate::runtime::Schema::strict).
    ///
    /// Common use case: typo protection on the typed part of a config plus
    /// a lenient subtree where third-party plugin keys land.
    ///
    /// `path` must resolve to a nested section in the schema; targeting a
    /// leaf or an unknown field produces
    /// [`ClapfigError::InvalidStrictPath`] from `build_resolver`. When
    /// [`normalize_keys(true)`](Self::normalize_keys) is set, `path` may be
    /// written in kebab-case — it is normalized before lookup.
    ///
    /// An empty `path` ( `""` ) targets the root, equivalent to
    /// [`strict(bool)`](Self::strict) but lets you still register
    /// per-subtree overrides.
    pub fn strict_at(mut self, path: &str, strict: bool) -> Self {
        self.strict_at_overrides.push((path.to_string(), strict));
        self
    }

    /// Register a per-key callback that runs on cascade-rejected unknown
    /// keys.
    ///
    /// The cascade rule decides strict/lenient first. If lenient, the key
    /// drops silently and the callback never runs. If strict and a callback
    /// is registered, the callback receives an
    /// [`UnknownKeyContext`](crate::UnknownKeyContext) carrying the dotted
    /// path, the leaf segment, the parsed value, the source file, and (for
    /// TOML sources) the line number — enough to apply a domain-specific
    /// decision. Returning
    /// [`UnknownKeyDecision::Accept`](crate::UnknownKeyDecision::Accept)
    /// drops the key silently;
    /// [`Reject`](crate::UnknownKeyDecision::Reject) produces a
    /// `ClapfigError::UnknownKeys` entry (same as no callback);
    /// [`Collect`](crate::UnknownKeyDecision::Collect) routes the key into
    /// [`load_with_unknowns`](Self::load_with_unknowns).
    pub fn on_unknown_key<F>(mut self, callback: F) -> Self
    where
        F: Fn(&crate::UnknownKeyContext<'_>) -> crate::UnknownKeyDecision + Send + Sync + 'static,
    {
        self.unknown_key_hook = Some(std::sync::Arc::new(callback));
        self
    }

    /// Convenience: register an [`on_unknown_key`](Self::on_unknown_key)
    /// callback implementing the "accept dotted, reject bare" pattern at a
    /// dotted-path subtree.
    ///
    /// Under `path` (and only there), any unknown key whose raw TOML leaf
    /// contains a `.` — typically a `[serde(flatten)]` extension key emitted
    /// by some other tool, like a quoted TOML literal
    /// `"acme.task-due-date-missing"` — is treated as a schema-extension key
    /// and `decision` decides its fate
    /// ([`UnknownKeyDecision::Accept`](crate::UnknownKeyDecision::Accept)
    /// drops it silently;
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect)
    /// routes it into
    /// [`load_with_unknowns`](Self::load_with_unknowns)). Bare unknown keys
    /// (no `.` in the leaf) fall through to
    /// [`UnknownKeyDecision::Reject`](crate::UnknownKeyDecision::Reject) —
    /// they look like typos, and the usual strict-mode error surfaces.
    ///
    /// `path` is bounded by segment: `"diag"` will not match keys under
    /// `"diagnostics.rules"`. Pass `""` to apply the rule everywhere.
    ///
    /// Calling this method replaces any previously-registered
    /// `on_unknown_key` callback (it is implemented in terms of one).
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

    /// Accept kebab-case keys in config files and CLI/URL overrides
    /// (default: `false`).
    ///
    /// When enabled, every key crossing the boundary into clapfig — TOML
    /// table keys, dotted CLI override keys, URL query parameter keys — has
    /// its `-` characters rewritten to `_` before validation, merging, and
    /// deserialization. snake_case keys continue to work unchanged; this is
    /// purely additive. Environment variables are unaffected.
    pub fn normalize_keys(mut self, normalize: bool) -> Self {
        self.normalize_keys = normalize;
        self
    }

    /// Set a custom layer merge order.
    ///
    /// Layers listed later override earlier ones. The default order is
    /// `[Files, Env, Url, Cli]` — the common-sense precedence where schema
    /// defaults are lowest and explicit overrides are highest.
    ///
    /// Omit a layer to exclude it from merging entirely. Duplicate layers
    /// are applied in the order given (the second occurrence overrides the
    /// first).
    pub fn layer_order(mut self, order: Vec<Layer>) -> Self {
        self.layer_order = Some(order);
        self
    }

    /// Post-merge validation hook. Receives the merged value [`Map`].
    ///
    /// Use it for constraints the schema can't express: numeric ranges,
    /// cross-field invariants ("if A is set then B must be set"), enum
    /// combinations, filesystem preconditions, anything that depends on the
    /// merged value rather than on a single field's type. Rejections are
    /// wrapped in [`ClapfigError::PostValidationFailed`]. Calling this
    /// method more than once replaces the previous hook. (The typed
    /// [`TypedBuilder::post_validate`](crate::TypedBuilder::post_validate)
    /// variant receives a typed `&C` instead.)
    pub fn post_validate<F>(mut self, f: F) -> Self
    where
        F: Fn(&Map) -> Result<(), String> + Send + Sync + 'static,
    {
        self.post_validate = Some(Box::new(move |t: &Map| {
            f(t).map_err(ClapfigError::PostValidationFailed)
        }));
        self
    }

    /// Internal variant of [`post_validate`](Self::post_validate) whose
    /// hook returns a full [`ClapfigError`]. Lets the typed wrapper
    /// ([`TypedBuilder::post_validate`](crate::TypedBuilder::post_validate))
    /// report its `Map → C` deserialize failure as the type error it is
    /// instead of a `PostValidationFailed` wearing a bare serde message.
    pub(crate) fn post_validate_raw<F>(mut self, f: F) -> Self
    where
        F: Fn(&Map) -> Result<(), ClapfigError> + Send + Sync + 'static,
    {
        self.post_validate = Some(Box::new(f));
        self
    }

    /// Add URL query parameters as a config layer.
    ///
    /// Parses the query string (e.g. `"port=9090&database.url=pg://prod"`)
    /// into config overrides. Keys use `.` for nesting, values are
    /// percent-decoded and parsed with the same heuristic as env vars
    /// (bool > int > float > string). A leading `?` is stripped if present.
    ///
    /// By default, URL parameters sit between env vars and CLI overrides in
    /// precedence: defaults < files < env < **URL** < CLI. This position can
    /// be changed with [`layer_order()`](Self::layer_order).
    #[cfg(feature = "url")]
    pub fn url_query(mut self, query: &str) -> Self {
        self.url_overrides
            .extend(crate::url::query_to_overrides(query));
        self
    }

    /// Add a CLI override. `None` values are ignored (useful for optional
    /// clap args).
    pub fn cli_override<V: Into<Value>>(mut self, key: &str, value: Option<V>) -> Self {
        if let Some(v) = value {
            self.cli_overrides.push((key.to_string(), v.into()));
        }
        self
    }

    /// Add CLI overrides from any serializable source, auto-matching by
    /// field name.
    ///
    /// Serializes `source` into flat key-value pairs, skips `None` values,
    /// and keeps only keys that match fields in the schema. Non-matching
    /// keys are silently ignored, so clap-only fields like `command` or
    /// `verbose` are automatically excluded. Composes with
    /// [`cli_override`](Self::cli_override) — both push to the same
    /// override list.
    pub fn cli_overrides_from<S: Serialize>(mut self, source: &S) -> Self {
        let pairs = flatten::flatten(source)
            .expect("clapfig: failed to flatten CLI source for auto-matching");
        let valid = overrides::valid_keys(&self.schema);
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

    fn effective_naming(&self) -> Result<FileNaming, ClapfigError> {
        if let Some(naming) = &self.file_naming {
            return Ok(naming.clone());
        }
        let app = self.effective_app_name()?;
        Ok(FileNaming::Exact(format!("{app}.toml")))
    }

    /// Build the enabled-formats registry for the effective file naming.
    ///
    /// - Exact-name mode enables only the name's extension's format.
    ///   Extensionless names fall back to TOML (matching the pre-registry
    ///   behavior for rc-style names); an extension no adapter claims is
    ///   a hard [`ClapfigError::UnknownFormat`] — never a silent TOML
    ///   fallback.
    /// - Stem mode enables the [`formats`](Self::formats) list in order
    ///   (default: TOML only). The list must be non-empty (the preferred
    ///   format is the first entry) and free of repeats (a repeated name
    ///   would collect the same file twice and misreport it as
    ///   ambiguous); violations are
    ///   [`ClapfigError::InvalidFormats`].
    fn effective_registry(&self) -> Result<FormatRegistry, ClapfigError> {
        let mut registry = FormatRegistry::new();
        match self.effective_naming()? {
            FileNaming::Exact(name) => {
                registry.register(adapter_for_explicit_path(Path::new(&name))?);
            }
            FileNaming::Stem(_) => {
                let names = self
                    .formats
                    .clone()
                    .unwrap_or_else(|| vec!["toml".to_string()]);
                if names.is_empty() {
                    return Err(ClapfigError::InvalidFormats {
                        reason: "formats(...) must enable at least one format".into(),
                    });
                }
                let mut seen: Vec<&str> = Vec::new();
                for name in &names {
                    if seen.contains(&name.as_str()) {
                        return Err(ClapfigError::InvalidFormats {
                            reason: format!("formats(...) names '{name}' more than once"),
                        });
                    }
                    seen.push(name);
                    let adapter = format::builtin_adapter(name).ok_or_else(|| {
                        ClapfigError::UnknownFormat {
                            name: name.clone(),
                            available: format::builtin_names(),
                        }
                    })?;
                    registry.register(adapter);
                }
            }
        }
        Ok(registry)
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

    /// Build a reusable [`Resolver`] that captures the current
    /// builder state and can be called repeatedly with
    /// [`resolve_at(dir)`](Resolver::resolve_at), each call
    /// interpreting [`SearchPath::Cwd`] and [`SearchPath::Ancestors`]
    /// relative to the passed directory.
    ///
    /// Use this when you need to resolve configuration at multiple points
    /// in a directory tree — for example, a static site generator visiting
    /// every content leaf, or a linter walking a repository. Files read
    /// from disk are cached inside the resolver so repeated walks pay the
    /// disk+parse cost once per unique file. Any
    /// [`post_validate`](Self::post_validate) hook registered on the
    /// builder is captured into the resolver and fires on every
    /// `resolve_at` call.
    ///
    /// Returns [`ClapfigError::AppNameRequired`] if `.app_name()` was not
    /// called on the builder.
    pub fn build_resolver(self) -> Result<Resolver, ClapfigError> {
        let app_name = self.effective_app_name()?.to_string();
        let naming = self.effective_naming()?;
        let registry = self.effective_registry()?;
        let search_paths = self.effective_search_paths();
        let env_prefix = self.effective_env_prefix()?;
        let env_vars = if env_prefix.is_some() {
            std::env::vars().collect()
        } else {
            Vec::new()
        };

        // Validate `strict_at` paths against the schema and merge with the
        // schema's own per-node `strict` settings into a single cascade
        // map.
        let strict_overrides = crate::strict::build_strict_overrides(
            &self.strict_at_overrides,
            self.normalize_keys,
            &self.schema,
        )?;

        Ok(Resolver {
            schema: self.schema,
            app_name,
            naming,
            registry,
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

    /// Load and resolve the configuration through all layers, returning
    /// the merged value [`Map`].
    ///
    /// If a [`post_validate`](Self::post_validate) hook is registered, it
    /// runs after the merged configuration has been produced and any
    /// rejection is returned as [`ClapfigError::PostValidationFailed`].
    ///
    /// Internally this is equivalent to
    /// `self.build_resolver()?.resolve_at(std::env::current_dir()?)`, so
    /// all resolution logic lives in exactly one place (see
    /// [`Resolver`]).
    pub fn load(self) -> Result<Map, ClapfigError> {
        let start_dir = std::env::current_dir().map_err(|e| ClapfigError::IoError {
            path: PathBuf::from("."),
            source: e,
        })?;
        self.build_resolver()?.resolve_at(start_dir)
    }

    /// Same as [`load`](Self::load) but also returns any keys the
    /// [`on_unknown_key`](Self::on_unknown_key) callback elected to
    /// [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    /// The list is empty when no callback is registered or no key opts in.
    pub fn load_with_unknowns(
        self,
    ) -> Result<(Map, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        let start_dir = std::env::current_dir().map_err(|e| ClapfigError::IoError {
            path: PathBuf::from("."),
            source: e,
        })?;
        self.build_resolver()?.resolve_at_with_unknowns(start_dir)
    }

    /// Dispatch a [`ConfigAction`] and print the result to stdout.
    ///
    /// Convenience wrapper around [`handle()`](Self::handle) for CLI apps
    /// that print directly. For programmatic use or integration with other
    /// output frameworks, prefer [`handle()`](Self::handle) (returns a
    /// [`ConfigResult`]) or [`handle_to_string()`](Self::handle_to_string).
    pub fn handle_and_print(self, action: &ConfigAction) -> Result<(), ClapfigError> {
        let result = self.handle(action)?;
        print!("{result}");
        Ok(())
    }

    /// Dispatch a [`ConfigAction`] and return the rendered output as a
    /// `String`.
    ///
    /// Like [`handle_and_print()`](Self::handle_and_print), but captures
    /// the output instead of printing to stdout.
    pub fn handle_to_string(self, action: &ConfigAction) -> Result<String, ClapfigError> {
        self.handle(action).map(|r| r.to_string())
    }

    /// Resolve the file path AND format adapter for a named persist
    /// scope.
    ///
    /// Exact-name scopes join the configured name onto the scope
    /// directory (the pre-registry behavior). Stem scopes apply the
    /// spec's `set` rules: exactly one existing same-stem file → edit
    /// that file in its own format; none → create
    /// `<stem>.<preferred extension>`; several → the same hard
    /// ambiguity error discovery raises. The adapter is then selected by
    /// the final path's extension (explicit-path rule): extensionless
    /// names fall back to TOML, and an unclaimed extension is a hard
    /// [`ClapfigError::UnknownFormat`].
    fn resolve_scope_persist_path(
        &self,
        scope: Option<&str>,
    ) -> Result<(PathBuf, Box<dyn FormatAdapter>), ClapfigError> {
        if self.persist_scopes.is_empty() {
            return Err(ClapfigError::NoPersistPath);
        }
        let app_name = self.effective_app_name()?;
        let naming = self.effective_naming()?;
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
        let path = match naming {
            FileNaming::Exact(name) => file::resolve_persist_path(search_path, &name, app_name)?,
            FileNaming::Stem(stem) => {
                let dir = match search_path {
                    SearchPath::Ancestors(_) => {
                        return Err(ClapfigError::AncestorsNotAllowedAsPersistPath);
                    }
                    other => file::resolve_search_path(other, app_name, None)
                        .ok_or(ClapfigError::NoPersistPath)?,
                };
                let registry = self.effective_registry()?;
                let mut matches: Vec<PathBuf> = Vec::new();
                for adapter in registry.iter() {
                    for ext in adapter.extensions() {
                        let candidate = dir.join(format!("{stem}.{ext}"));
                        if candidate.is_file() {
                            matches.push(candidate);
                        }
                    }
                }
                match matches.len() {
                    0 => {
                        let preferred = registry
                            .preferred()
                            .expect("effective_registry always registers an adapter");
                        dir.join(format!("{stem}.{}", preferred.extensions()[0]))
                    }
                    1 => matches.remove(0),
                    _ => {
                        return Err(ClapfigError::AmbiguousConfigFiles {
                            dir,
                            files: matches,
                        });
                    }
                }
            }
        };
        let adapter = adapter_for_explicit_path(&path)?;
        Ok((path, adapter))
    }

    /// Dispatch a [`ConfigAction`] against the schema.
    ///
    /// Returns a [`ConfigResult`]; the typed path's `handle` delegates
    /// here, so downstream rendering / printing code is shared.
    pub fn handle(self, action: &ConfigAction) -> Result<ConfigResult, ClapfigError> {
        match action {
            ConfigAction::List { scope } => match scope {
                None => {
                    // The merged view spans formats; display renders in
                    // the preferred (first-enabled) format's spelling.
                    let registry = self.effective_registry()?;
                    let table = self.load()?;
                    Ok(list_from_table(
                        &table,
                        registry
                            .preferred()
                            .expect("effective_registry always registers an adapter"),
                    ))
                }
                Some(name) => {
                    let (path, adapter) = self.resolve_scope_persist_path(Some(name))?;
                    ops::list_scope_file(adapter.as_ref(), &path)
                }
            },
            ConfigAction::Gen { output } => {
                let registry = self.effective_registry()?;
                match output {
                    Some(path) => {
                        // Explicit output path: the extension selects the
                        // adapter, independent of the enabled list — an
                        // extension no adapter claims is a hard
                        // UnknownFormat error. Only a genuinely
                        // extensionless path falls back to the preferred
                        // format.
                        let adapter = match path.extension() {
                            None => {
                                let preferred = registry
                                    .preferred()
                                    .expect("effective_registry always registers an adapter");
                                format::builtin_adapter(preferred.name())
                                    .expect("preferred adapters are built in")
                            }
                            Some(_) => adapter_for_explicit_path(path)?,
                        };
                        let template = ops::generate_template(
                            adapter.as_ref(),
                            &self.schema,
                            self.normalize_keys,
                        )?;
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
                    None => {
                        // Stdout gen renders the preferred (first-enabled)
                        // format.
                        let preferred = registry
                            .preferred()
                            .expect("effective_registry always registers an adapter");
                        let template =
                            ops::generate_template(preferred, &self.schema, self.normalize_keys)?;
                        Ok(ConfigResult::Template(template))
                    }
                }
            }
            ConfigAction::Schema { output } => {
                let value = crate::json_schema::generate_schema(&self.schema);
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
                    let schema = Arc::clone(&self.schema);
                    let normalize_keys = self.normalize_keys;
                    // Merged view: display renders in the preferred
                    // (first-enabled) format's spelling.
                    let registry = self.effective_registry()?;
                    let table = self.load()?;
                    get_from_table(
                        &schema,
                        &table,
                        key,
                        normalize_keys,
                        registry
                            .preferred()
                            .expect("effective_registry always registers an adapter"),
                    )
                }
                Some(name) => {
                    let (path, adapter) = self.resolve_scope_persist_path(Some(name))?;
                    get_scope(
                        adapter.as_ref(),
                        &self.schema,
                        name,
                        &path,
                        key,
                        self.normalize_keys,
                    )
                }
            },
            ConfigAction::Set { key, value, scope } => {
                let (path, adapter) = self.resolve_scope_persist_path(scope.as_deref())?;
                persist::persist_value(
                    adapter.as_ref(),
                    &self.schema,
                    &path,
                    key,
                    value,
                    self.normalize_keys,
                )
            }
            ConfigAction::Unset { key, scope } => {
                let (path, adapter) = self.resolve_scope_persist_path(scope.as_deref())?;
                crate::persist::unset_value(adapter.as_ref(), &path, key, self.normalize_keys)
            }
        }
    }
}

/// Reusable resolution handle for tree-walk use cases.
///
/// Built once via [`Builder::build_resolver`], then called
/// repeatedly with [`resolve_at(dir)`](Resolver::resolve_at) to
/// produce a merged configuration anchored at a specific directory. This
/// unlocks the `.htaccess` / `.gitignore` / `.editorconfig` pattern: a
/// dynamic file tree where every directory is its own resolution root,
/// each leaf producing an independently merged configuration. See the
/// [crate-level "Tree-walk resolution" section](crate#tree-walk-resolution--the-resolver-handle)
/// for the full design rationale.
pub struct Resolver {
    schema: Arc<Schema>,
    app_name: String,
    naming: FileNaming,
    registry: FormatRegistry,
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
    post_validate: Option<Arc<PostValidateHook>>,
    file_cache: Mutex<std::collections::HashMap<PathBuf, String>>,
}

impl Resolver {
    pub fn resolve_at(&self, start_dir: impl AsRef<std::path::Path>) -> Result<Map, ClapfigError> {
        self.resolve_at_inner(start_dir.as_ref())
            .map(|(table, _unknowns)| table)
    }

    /// Same as [`resolve_at`](Self::resolve_at) but also returns any keys
    /// the [`on_unknown_key`](Builder::on_unknown_key)
    /// callback elected to [`UnknownKeyDecision::Collect`](crate::UnknownKeyDecision::Collect).
    pub fn resolve_at_with_unknowns(
        &self,
        start_dir: impl AsRef<std::path::Path>,
    ) -> Result<(Map, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
        self.resolve_at_inner(start_dir.as_ref())
    }

    /// Shared implementation behind [`resolve_at`](Self::resolve_at) and
    /// [`resolve_at_with_unknowns`](Self::resolve_at_with_unknowns): one
    /// place owns anchoring, discovery, caching, resolution, and the
    /// post-validate hook, so the two public surfaces stay thin wrappers
    /// that only differ in whether the collected-unknowns list is kept
    /// or dropped.
    fn resolve_at_inner(
        &self,
        start_dir: &std::path::Path,
    ) -> Result<(Map, Vec<crate::strict::CollectedUnknown>), ClapfigError> {
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
            schema: self.schema.as_ref(),
            registry: &self.registry,
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
            hook(&table)?;
        }
        Ok((table, unknowns))
    }

    fn load_files_cached(&self, dirs: &[PathBuf]) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
        match self.search_mode {
            SearchMode::Merge => {
                let mut out = Vec::new();
                for dir in dirs {
                    if let Some(found) = self.find_in_dir(dir)? {
                        out.push(found);
                    }
                }
                Ok(out)
            }
            SearchMode::FirstMatch => {
                for dir in dirs.iter().rev() {
                    if let Some(found) = self.find_in_dir(dir)? {
                        return Ok(vec![found]);
                    }
                }
                Ok(Vec::new())
            }
        }
    }

    /// Discover this resolver's config file inside one directory.
    ///
    /// Exact naming probes the single configured name. Stem naming probes
    /// `<stem>.<ext>` across every enabled adapter's extensions; more
    /// than one hit in the same directory is the spec's hard
    /// [`AmbiguousConfigFiles`](ClapfigError::AmbiguousConfigFiles) error
    /// (no silent precedence, no merging of same-stem siblings).
    fn find_in_dir(&self, dir: &Path) -> Result<Option<(PathBuf, String)>, ClapfigError> {
        match &self.naming {
            FileNaming::Exact(name) => {
                let path = dir.join(name);
                Ok(self.read_cached(&path)?.map(|contents| (path, contents)))
            }
            FileNaming::Stem(stem) => {
                let mut found: Vec<(PathBuf, String)> = Vec::new();
                for adapter in self.registry.iter() {
                    for ext in adapter.extensions() {
                        let path = dir.join(format!("{stem}.{ext}"));
                        if let Some(contents) = self.read_cached(&path)? {
                            found.push((path, contents));
                        }
                    }
                }
                match found.len() {
                    0 => Ok(None),
                    1 => Ok(found.pop()),
                    _ => Err(ClapfigError::AmbiguousConfigFiles {
                        dir: dir.to_path_buf(),
                        files: found.into_iter().map(|(p, _)| p).collect(),
                    }),
                }
            }
        }
    }

    /// Number of files currently held in the resolver's cache. Intended for
    /// tests and diagnostics; production code should not branch on this.
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

/// Select the format adapter for an explicit file path by its extension
/// (independent of the enabled-formats list). Extensionless names fall
/// back to TOML (the rc-style preservation rule); an extension no adapter
/// claims is a hard [`ClapfigError::UnknownFormat`] — never a silent TOML
/// fallback that would write or parse one format's content under another
/// format's extension.
fn adapter_for_explicit_path(path: &Path) -> Result<Box<dyn FormatAdapter>, ClapfigError> {
    match path.extension() {
        None => Ok(format::builtin_adapter("toml").expect("toml adapter is built in")),
        Some(ext) => {
            let ext = ext.to_string_lossy();
            format::builtin_adapter_for_extension(&ext).ok_or_else(|| ClapfigError::UnknownFormat {
                name: ext.into_owned(),
                available: format::builtin_names(),
            })
        }
    }
}

/// Render every leaf in a resolved table as flat dotted-key entries — the
/// `config list` output shape. Display lines are spelled by `adapter`
/// (the active format).
fn list_from_table(table: &Map, adapter: &dyn FormatAdapter) -> ConfigResult {
    let mut entries = Vec::new();
    flatten_table(table, "", &mut entries);
    ConfigResult::listing(adapter, entries)
}

fn flatten_table(table: &Map, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Map(t) => flatten_table(t, &full, out),
            _ => out.push((full, format_leaf_value(value))),
        }
    }
}

fn format_leaf_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => crate::value::lexical_string(d),
        // Containers render in the value model's deterministic inline
        // notation.
        Value::Array(_) | Value::Map(_) => value.to_string(),
    }
}

/// `config get` against the merged table. The merged table's keys are
/// canonical snake_case (the load path normalized them), so with
/// `normalize_keys` the action key is normalized before lookup — a kebab
/// action key finds its snake entry. The reported key keeps the caller's
/// spelling; the display block is spelled by `adapter` (the active
/// format).
fn get_from_table(
    schema: &Schema,
    table: &Map,
    key: &str,
    normalize_keys: bool,
    adapter: &dyn FormatAdapter,
) -> Result<ConfigResult, ClapfigError> {
    let canonical = if normalize_keys {
        crate::normalize::normalize_key(key)
    } else {
        key.to_owned()
    };
    let value = ops::table_get(table, &canonical).ok_or_else(|| ClapfigError::KeyNotFound {
        key: key.into(),
        suggestion: crate::meta::nearest_key(schema, &canonical),
    })?;
    let doc = crate::meta::doc_for(schema, &canonical).unwrap_or_default();
    Ok(ConfigResult::key_value(
        adapter,
        key.into(),
        format_leaf_value(value),
        doc,
    ))
}

/// Scoped `config get`: reads one scope's raw (un-normalized) file. With
/// `normalize_keys`, the action key is normalized to the canonical
/// snake_case path and looked up by dash/underscore equivalence
/// ([`ops::table_get_normalized`]), so a kebab-case document answers for
/// either action-key spelling — and a document holding BOTH equivalent
/// spellings anywhere (even at keys the lookup never touches) fails as
/// [`ClapfigError::NormalizedKeyCollision`] instead of answering from a
/// document the load path refuses. The reported key keeps the caller's
/// spelling; the display block is spelled by `adapter` (the scope file's
/// format). A scope whose file does not exist fails as
/// [`ClapfigError::ScopeFileMissing`] naming the scope and the file —
/// the key may be perfectly valid; there is just nothing to read.
fn get_scope(
    adapter: &dyn FormatAdapter,
    schema: &Schema,
    scope: &str,
    file_path: &std::path::Path,
    key: &str,
    normalize_keys: bool,
) -> Result<ConfigResult, ClapfigError> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClapfigError::ScopeFileMissing {
                scope: scope.into(),
                path: file_path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(ClapfigError::IoError {
                path: file_path.to_path_buf(),
                source: e,
            });
        }
    };

    let table = match adapter
        .parse(&content)
        .map_err(|e| ClapfigError::ParseError {
            path: file_path.to_path_buf(),
            source: Box::new(e),
            source_text: Some(Arc::from(content.as_str())),
        })? {
        Value::Map(map) => map,
        other => {
            return Err(ClapfigError::InvalidValue {
                key: file_path.display().to_string(),
                reason: format!(
                    "config documents must be maps at the root, got {}",
                    other.type_str()
                ),
            });
        }
    };

    let (canonical, value) = if normalize_keys {
        let canonical = crate::normalize::normalize_key(key);
        let value =
            ops::table_get_normalized(&table, &canonical).map_err(|c| c.into_error(file_path))?;
        (canonical, value)
    } else {
        (key.to_owned(), ops::table_get(&table, key))
    };
    let value = value.ok_or_else(|| ClapfigError::KeyNotFound {
        key: key.into(),
        suggestion: crate::meta::nearest_key(schema, &canonical),
    })?;
    let doc = crate::meta::doc_for(schema, &canonical).unwrap_or_default();
    Ok(ConfigResult::key_value(
        adapter,
        key.into(),
        format_leaf_value(value),
        doc,
    ))
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
        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("level"), Some(&Value::String("info".into())));
        let db = table.get("db").and_then(Value::as_map).unwrap();
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

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("port"), Some(&Value::Integer(9090)));
        let db = table.get("db").and_then(Value::as_map).unwrap();
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

        let table = Clapfig::builder(demo_schema())
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
        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_override("port", Some(11111i64))
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(11111)));
    }

    // --- search-path composition (guards from the FIXME.md audit) ---

    /// Guard (FIXME.md #1/#10, test deleted by #105): `add_search_path`
    /// as the FIRST path call must start from the default `[Platform]`,
    /// not an empty list — "append without replacing" includes the
    /// implicit default.
    #[test]
    fn add_search_path_first_call_preserves_platform_default() {
        let builder = Clapfig::builder(demo_schema())
            .app_name("demo")
            .add_search_path(SearchPath::Cwd);
        assert_eq!(
            builder.effective_search_paths(),
            vec![SearchPath::Platform, SearchPath::Cwd]
        );
    }

    /// Guard (FIXME.md #10): `add_search_path` after an explicit
    /// `search_paths` list appends to that list.
    #[test]
    fn add_search_path_appends_to_explicit_list() {
        let builder = Clapfig::builder(demo_schema())
            .app_name("demo")
            .search_paths(vec![SearchPath::Home(".demo")])
            .add_search_path(SearchPath::Cwd);
        assert_eq!(
            builder.effective_search_paths(),
            vec![SearchPath::Home(".demo"), SearchPath::Cwd]
        );
    }

    /// Guard (FIXME.md #13, test dropped by an earlier resolver
    /// refactor): an unreadable (permission-denied) config file is a hard
    /// IO error naming the file — only a MISSING file is silently
    /// skipped.
    #[cfg(unix)]
    #[test]
    fn unreadable_config_file_errors_instead_of_being_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("demo.toml");
        fs::write(&path, "port = 9090\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&path).is_ok() {
            // Running as root: permissions don't bind, nothing to test.
            return;
        }

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();
        match result {
            Err(ClapfigError::IoError { path: reported, .. }) => assert_eq!(reported, path),
            other => panic!("expected IoError, got {other:?}"),
        }
    }

    // --- strict / unknown keys ---

    #[test]
    fn strict_rejects_unknown_top_level_with_line_number() {
        let dir = TempDir::new().unwrap();
        let source = "port = 8080\ntypo = 1\n";
        fs::write(dir.path().join("demo.toml"), source).unwrap();

        let result = Clapfig::builder(demo_schema())
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

        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(demo_schema())
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

        let result = Clapfig::builder(schema)
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

        let _ = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .post_validate(move |t: &Map| {
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
        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(schema)
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
        let parsed = crate::fixtures::test::parse_toml(&t);
        assert!(parsed.contains_key("first"), "first must be at root:\n{t}");
        assert!(
            parsed.contains_key("second"),
            "second leaked into [inner] (template ordering bug):\n{t}"
        );
        let inner = parsed.get("inner").and_then(|v| v.as_map()).unwrap();
        assert!(
            inner.get("second").is_none(),
            "second must not be under inner"
        );
    }

    #[test]
    fn handle_gen_renders_template_with_doc_comments_and_enum_set() {
        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(schema)
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
        let result = Clapfig::builder(schema)
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

        let table = Clapfig::builder(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();

        let rules = table["rules"].as_map().unwrap();
        assert_eq!(rules["missing_footnote"].as_str(), Some("warn"));
        assert!(rules["bad_columns"].as_array().is_some());
    }

    #[test]
    fn handle_schema_does_not_mark_array_of_required() {
        // Regression: finalization accepts an absent ArrayOf as
        // the empty list. The JSON Schema must agree — marking the
        // property required would reject configs clapfig itself accepts.
        let schema = Schema::object("Top")
            .field("name", RtField::string().default("x"))
            .array_of(
                "plugins",
                Schema::object("Plugin").field("id", RtField::string()),
            )
            .build();
        let result = Clapfig::builder(schema)
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
        let table = Clapfig::builder(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        let plugins = table["plugins"].as_map().unwrap();
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
        let table = Clapfig::builder(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        let plugins = table["plugins"].as_map().unwrap();
        assert!(
            !plugins["audit"].as_map().unwrap()["enabled"]
                .as_bool()
                .unwrap(),
            "missing leaf in map entry must get the default"
        );
        assert!(
            plugins["fmt"].as_map().unwrap()["enabled"]
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
        let result = Clapfig::builder(map_of_schema())
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
        let err = Clapfig::builder(map_of_schema())
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
        let table = Clapfig::builder(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        // `plugins` may or may not be present in the resulting table;
        // what matters is that load doesn't error.
        if let Some(plugins) = table.get("plugins") {
            let plugins_table = plugins.as_map().unwrap();
            assert!(plugins_table.is_empty());
        }
    }

    #[test]
    fn map_of_json_schema_emits_additional_properties() {
        let result = Clapfig::builder(map_of_schema())
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
        // it must be a map-of-maps; loading must error.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "plugins = \"oops\"\n").unwrap();
        let result = Clapfig::builder(map_of_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load();
        match result.unwrap_err() {
            ClapfigError::InvalidValue { key, reason } => {
                assert_eq!(key, "plugins");
                assert!(reason.contains("expected map"));
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
        // "expected array, got map". ArrayOf subtrees stay excluded from
        // `valid_keys`; the user-facing symptom is the targeted
        // `UnaddressableKey` refusal pointing at the config file — never
        // a corrupted file, and no longer a bare `KeyNotFound`.
        let dir = TempDir::new().unwrap();
        let schema = Schema::object("Top").array_of(
            "plugins",
            Schema::object("Plugin").field("id", RtField::string()),
        );
        let result = Clapfig::builder(schema.build())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "plugins.id".into(),
                value: "x".into(),
                scope: None,
            });
        match result {
            Err(ClapfigError::UnaddressableKey { section, kind, .. }) => {
                assert_eq!(section, "plugins");
                assert_eq!(kind, "an array");
            }
            other => panic!("expected UnaddressableKey for ArrayOf-internal key, got {other:?}"),
        }
        // File must not have been written.
        assert!(!dir.path().join("demo.toml").exists());
    }

    #[test]
    fn handle_schema_emits_enum_array_and_descriptions() {
        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(demo_schema())
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
        let result = Clapfig::builder(demo_schema())
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
        let table = Clapfig::builder(demo_schema())
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
        assert!(!table.contains_key("verbose"));
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
        let table = Clapfig::builder(schema)
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
                .and_then(|v| v.as_map())
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
        let result = Clapfig::builder(schema)
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
        let result = Clapfig::builder(schema)
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
        let result = Clapfig::builder(schema)
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
        let result = Clapfig::builder(dotted_ext_schema())
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
        let result = Clapfig::builder(dotted_ext_schema())
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
        let result = Clapfig::builder(dotted_ext_schema())
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
        let (_table, unknowns) = Clapfig::builder(dotted_ext_schema())
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
        let result = Clapfig::builder(schema)
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

    // --- Resolver cache behavior ---

    fn resolver_with_path(dir: &std::path::Path) -> Resolver {
        Clapfig::builder(demo_schema())
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
        // freshness."
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

        let resolver = Clapfig::builder(demo_schema())
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

    // --- search modes ---

    #[test]
    fn first_match_uses_highest_priority_file_only() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(
            dir1.path().join("demo.toml"),
            "port = 1000\nhost = \"low\"\n",
        )
        .unwrap();
        fs::write(dir2.path().join("demo.toml"), "port = 2000\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()), // highest priority
            ])
            .search_mode(SearchMode::FirstMatch)
            .no_env()
            .load()
            .unwrap();

        // Should use dir2 only — port from dir2, host from defaults (not dir1!)
        assert_eq!(table.get("port"), Some(&Value::Integer(2000)));
        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));
    }

    #[test]
    fn merge_mode_combines_both_files() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(
            dir1.path().join("demo.toml"),
            "port = 1000\nhost = \"base\"\n",
        )
        .unwrap();
        fs::write(dir2.path().join("demo.toml"), "port = 2000\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()),
            ])
            .search_mode(SearchMode::Merge)
            .no_env()
            .load()
            .unwrap();

        // Merge: port from dir2 (higher priority), host from dir1 (lower priority)
        assert_eq!(table.get("port"), Some(&Value::Integer(2000)));
        assert_eq!(table.get("host"), Some(&Value::String("base".into())));
    }

    #[test]
    fn first_match_falls_back_when_high_priority_missing() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        // Only dir1 (lower priority) has a config
        fs::write(dir1.path().join("demo.toml"), "port = 1000\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![
                SearchPath::Path(dir1.path().to_path_buf()),
                SearchPath::Path(dir2.path().to_path_buf()),
            ])
            .search_mode(SearchMode::FirstMatch)
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("port"), Some(&Value::Integer(1000)));
    }

    #[test]
    fn missing_app_name_errors() {
        let result = Clapfig::builder(demo_schema()).no_env().load();
        assert!(matches!(result, Err(ClapfigError::AppNameRequired)));
    }

    // --- layer order (builder wiring; pipeline-level coverage in resolve.rs) ---

    #[test]
    fn layer_order_cli_below_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 3000\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_override("port", Some(9999i64))
            .layer_order(vec![Layer::Cli, Layer::Files])
            .load()
            .unwrap();

        // Files listed after Cli, so the file wins.
        assert_eq!(table.get("port"), Some(&Value::Integer(3000)));
    }

    // --- normalize_keys (builder wiring; pipeline-level coverage in resolve.rs) ---

    #[test]
    fn normalize_keys_load_accepts_kebab_in_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "[db]\npool-size = 42\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .normalize_keys(true)
            .load()
            .unwrap();

        let db = table.get("db").and_then(Value::as_map).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(42)));
    }

    #[test]
    fn handle_set_normalized_roundtrip_has_no_collision() {
        // End-to-end regression for #122: with normalize_keys on, set
        // seeds a kebab file, later sets in EITHER spelling edit that
        // same key, and the file stays loadable (no
        // NormalizedKeyCollision from a snake/kebab duplicate).
        use crate::format::FormatAdapter as _;
        use crate::format::TomlAdapter;

        let dir = TempDir::new().unwrap();
        let builder = || {
            Clapfig::builder(demo_schema())
                .app_name("demo")
                .file_name("demo.toml")
                .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
                .no_env()
                .normalize_keys(true)
        };
        let file = dir.path().join("demo.toml");
        let db_map = || {
            let content = fs::read_to_string(&file).unwrap();
            let tree = TomlAdapter.parse(&content).unwrap();
            tree.as_map().unwrap()["db"].as_map().unwrap().clone()
        };

        // Missing file: seeded from the kebab template, snake action key.
        builder()
            .handle(&ConfigAction::Set {
                key: "db.pool_size".into(),
                value: "10".into(),
                scope: None,
            })
            .unwrap();
        let db = db_map();
        assert_eq!(db.get("pool-size"), Some(&Value::Integer(10)));
        assert!(!db.contains_key("pool_size"));

        // Existing kebab file: both spellings edit the same key.
        builder()
            .handle(&ConfigAction::Set {
                key: "db.pool-size".into(),
                value: "11".into(),
                scope: None,
            })
            .unwrap();
        builder()
            .handle(&ConfigAction::Set {
                key: "db.pool_size".into(),
                value: "12".into(),
                scope: None,
            })
            .unwrap();
        let db = db_map();
        assert_eq!(db.get("pool-size"), Some(&Value::Integer(12)));
        assert!(!db.contains_key("pool_size"));

        // The file the persistence path wrote loads cleanly.
        let table = builder().load().unwrap();
        let db = table.get("db").and_then(Value::as_map).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(12)));

        // Unset in the snake spelling removes the kebab entry; the
        // default shows through again on load.
        builder()
            .handle(&ConfigAction::Unset {
                key: "db.pool_size".into(),
                scope: None,
            })
            .unwrap();
        let db = db_map();
        assert!(!db.contains_key("pool-size") && !db.contains_key("pool_size"));
        let table = builder().load().unwrap();
        let db = table.get("db").and_then(Value::as_map).unwrap();
        assert_eq!(db.get("pool_size"), Some(&Value::Integer(5)));
    }

    #[test]
    fn scoped_get_accepts_both_spellings_on_normalized_files() {
        // Scoped get reads the raw (un-normalized) file, so a kebab
        // document must answer for either action-key spelling — across
        // all three formats.
        use crate::format::{FormatAdapter, JsonAdapter, TomlAdapter, YamlAdapter};

        let dir = TempDir::new().unwrap();
        let files: [(&str, &dyn FormatAdapter, &str); 3] = [
            ("demo.toml", &TomlAdapter, "[db]\npool-size = 42\n"),
            ("demo.yaml", &YamlAdapter, "db:\n  pool-size: 42\n"),
            ("demo.json", &JsonAdapter, "{\"db\": {\"pool-size\": 42}}"),
        ];
        for (name, adapter, content) in files {
            let path = dir.path().join(name);
            fs::write(&path, content).unwrap();
            for key in ["db.pool-size", "db.pool_size"] {
                let result = get_scope(adapter, &demo_schema(), "local", &path, key, true).unwrap();
                match result {
                    ConfigResult::KeyValue {
                        key: reported,
                        value,
                        ..
                    } => {
                        assert_eq!(value, "42", "{name} / {key}");
                        assert_eq!(reported, key, "reported key keeps the caller's spelling");
                    }
                    other => panic!("expected KeyValue, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn scoped_get_rejects_ambiguous_spellings_as_collision() {
        // A raw file holding BOTH equivalent spellings is ambiguous — the
        // load path refuses it — so scoped get errors with the same
        // collision (stamped with the scope file's path) instead of one
        // spelling silently answering.
        use crate::format::TomlAdapter;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("demo.toml");
        fs::write(&path, "[db]\npool-size = 5\npool_size = 6\n").unwrap();
        for key in ["db.pool-size", "db.pool_size"] {
            let err =
                get_scope(&TomlAdapter, &demo_schema(), "local", &path, key, true).unwrap_err();
            match err {
                ClapfigError::NormalizedKeyCollision {
                    path: reported,
                    section,
                    normalized_key,
                    originals,
                } => {
                    assert_eq!(reported, path, "{key}");
                    assert_eq!(section, "db");
                    assert_eq!(normalized_key, "pool_size");
                    assert_eq!(originals, vec!["pool-size", "pool_size"]);
                }
                other => panic!("expected NormalizedKeyCollision, got {other:?}"),
            }
        }
    }

    #[test]
    fn scoped_get_rejects_collision_off_the_requested_path() {
        // Whole-document validation: `host` itself is unambiguous, but
        // the file holds both spellings in the untraversed `db` table —
        // scoped get still fails, because a document the load path
        // refuses is never queried.
        use crate::format::TomlAdapter;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("demo.toml");
        fs::write(
            &path,
            "host = \"h\"\n\n[db]\npool-size = 5\npool_size = 6\n",
        )
        .unwrap();
        let err =
            get_scope(&TomlAdapter, &demo_schema(), "local", &path, "host", true).unwrap_err();
        match err {
            ClapfigError::NormalizedKeyCollision {
                path: reported,
                section,
                normalized_key,
                ..
            } => {
                assert_eq!(reported, path);
                assert_eq!(section, "db");
                assert_eq!(normalized_key, "pool_size");
            }
            other => panic!("expected NormalizedKeyCollision, got {other:?}"),
        }
    }

    #[test]
    fn scoped_get_missing_file_names_scope_and_file_not_the_key() {
        // The key may be perfectly valid — there is simply no file to
        // read. Claiming "key not found" would send the user hunting for
        // a typo that isn't there.
        use crate::format::TomlAdapter;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.toml");
        for key in ["db.pool-size", "db.pool_size"] {
            let err =
                get_scope(&TomlAdapter, &demo_schema(), "local", &path, key, true).unwrap_err();
            match err {
                ClapfigError::ScopeFileMissing {
                    scope,
                    path: reported,
                } => {
                    assert_eq!(scope, "local");
                    assert_eq!(reported, path);
                }
                other => panic!("expected ScopeFileMissing, got {other:?}"),
            }
        }
    }

    #[test]
    fn get_unknown_key_suggests_near_miss() {
        let dir = TempDir::new().unwrap();
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Get {
                key: "db.pool_sizr".into(),
                scope: None,
            })
            .unwrap_err();
        match err {
            ClapfigError::KeyNotFound { key, suggestion } => {
                assert_eq!(key, "db.pool_sizr");
                assert_eq!(suggestion.as_deref(), Some("db.pool_size"));
            }
            other => panic!("expected KeyNotFound, got {other:?}"),
        }
    }

    #[test]
    fn handle_get_merged_accepts_kebab_key_with_normalization() {
        // The merged table's keys are canonical snake_case after the
        // load-path normalization; the action key follows the same
        // acceptance, so `get pool-size` answers from the merged view.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "[db]\npool-size = 42\n").unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .normalize_keys(true)
            .handle(&ConfigAction::Get {
                key: "db.pool-size".into(),
                scope: None,
            })
            .unwrap();

        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "42"),
            other => panic!("expected KeyValue, got {other:?}"),
        }
    }

    // --- handle: template + schema output variants ---

    #[test]
    fn handle_gen_kebab_emits_kebab_keys() {
        // End-to-end: a builder with .normalize_keys(true) must produce a
        // template whose keys match what users will type, not the schema
        // field names.
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .no_env()
            .normalize_keys(true)
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();

        match result {
            ConfigResult::Template(t) => {
                assert!(
                    t.contains("pool-size"),
                    "expected kebab key in generated template:\n{t}"
                );
                assert!(
                    !t.contains("pool_size"),
                    "snake form leaked into normalize_keys template:\n{t}"
                );
            }
            other => panic!("Expected Template, got {other:?}"),
        }
    }

    #[test]
    fn handle_gen_default_still_snake() {
        // Without normalize_keys, the template stays snake_case — defaults
        // unchanged for callers that haven't opted in.
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Gen { output: None })
            .unwrap();

        match result {
            ConfigResult::Template(t) => {
                assert!(t.contains("pool_size"));
                assert!(!t.contains("pool-size"));
            }
            other => panic!("Expected Template, got {other:?}"),
        }
    }

    #[test]
    fn handle_gen_with_output() {
        let dir = TempDir::new().unwrap();
        let out_path = dir.path().join("generated.toml");

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
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
    fn handle_schema_with_output() {
        let dir = TempDir::new().unwrap();
        let out_path = dir.path().join("schema.json");

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Schema {
                output: Some(out_path.clone()),
            })
            .unwrap();

        assert!(matches!(result, ConfigResult::SchemaWritten { .. }));
        let content = fs::read_to_string(&out_path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["title"], "App");
    }

    // --- persist scopes ---

    #[test]
    fn handle_set_requires_persist_scope() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: None,
            });

        assert!(matches!(result, Err(ClapfigError::NoPersistPath)));
    }

    #[test]
    fn handle_unset_requires_persist_scope() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .handle(&ConfigAction::Unset {
                key: "port".into(),
                scope: None,
            });

        assert!(matches!(result, Err(ClapfigError::NoPersistPath)));
    }

    #[test]
    fn handle_set_rejects_ancestors_scope() {
        use crate::types::Boundary;
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .persist_scope("bad", SearchPath::Ancestors(Boundary::Root))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: None,
            });

        assert!(matches!(
            result,
            Err(ClapfigError::AncestorsNotAllowedAsPersistPath)
        ));
    }

    #[test]
    fn handle_unknown_scope_errors() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: Some("nonexistent".into()),
            });

        match result {
            Err(ClapfigError::UnknownScope { scope, available }) => {
                assert_eq!(scope, "nonexistent");
                assert_eq!(available, vec!["local"]);
            }
            other => panic!("Expected UnknownScope, got {other:?}"),
        }
    }

    #[test]
    fn handle_list_with_scope() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 3000\n").unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::List {
                scope: Some("local".into()),
            })
            .unwrap();

        match result {
            ConfigResult::Listing { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0], ("port".into(), "3000".into()));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn handle_get_with_scope() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 3000\n").unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Get {
                key: "port".into(),
                scope: Some("local".into()),
            })
            .unwrap();

        match result {
            ConfigResult::KeyValue { value, .. } => assert_eq!(value, "3000"),
            other => panic!("Expected KeyValue, got {other:?}"),
        }
    }

    #[test]
    fn multiple_scopes_separate_files() {
        let local_dir = TempDir::new().unwrap();
        let global_dir = TempDir::new().unwrap();

        let make_builder = || {
            Clapfig::builder(demo_schema())
                .app_name("demo")
                .file_name("demo.toml")
                .persist_scope("local", SearchPath::Path(local_dir.path().to_path_buf()))
                .persist_scope("global", SearchPath::Path(global_dir.path().to_path_buf()))
                .no_env()
        };

        // Set in local (default scope)
        make_builder()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "3000".into(),
                scope: None,
            })
            .unwrap();

        // Set in global
        make_builder()
            .handle(&ConfigAction::Set {
                key: "host".into(),
                value: "0.0.0.0".into(),
                scope: Some("global".into()),
            })
            .unwrap();

        // Verify separate files
        let local_content = fs::read_to_string(local_dir.path().join("demo.toml")).unwrap();
        assert!(local_content.contains("port = 3000"));
        assert!(!local_content.contains("host = \"0.0.0.0\""));

        let global_content = fs::read_to_string(global_dir.path().join("demo.toml")).unwrap();
        assert!(global_content.contains("host = \"0.0.0.0\""));
        assert!(!global_content.contains("port = 3000"));

        // List scoped: only that file's explicitly-set entries (the seeded
        // template contributes the defaults, so filter to the set key).
        let local_list = make_builder()
            .handle(&ConfigAction::List {
                scope: Some("local".into()),
            })
            .unwrap();
        match local_list {
            ConfigResult::Listing { entries, .. } => {
                assert!(entries.iter().any(|(k, v)| k == "port" && v == "3000"));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }

        // List merged (no scope): sees both files merged + defaults
        let merged_list = make_builder()
            .handle(&ConfigAction::List { scope: None })
            .unwrap();
        match merged_list {
            ConfigResult::Listing { entries, .. } => {
                let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
                assert!(keys.contains(&"port"));
                assert!(keys.contains(&"host"));
            }
            other => panic!("Expected Listing, got {other:?}"),
        }
    }

    #[test]
    fn persist_scope_auto_added_to_search_paths() {
        // A value set through a persist scope must be discoverable on the
        // next load even when the scope dir was never listed in
        // search_paths.
        let scope_dir = TempDir::new().unwrap();

        Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .persist_scope("local", SearchPath::Path(scope_dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "4242".into(),
                scope: None,
            })
            .unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![])
            .persist_scope("local", SearchPath::Path(scope_dir.path().to_path_buf()))
            .no_env()
            .load()
            .unwrap();

        assert_eq!(table.get("port"), Some(&Value::Integer(4242)));
    }

    // --- strict_at path validation ---

    #[test]
    fn strict_at_invalid_path_errors_at_build_resolver() {
        // Path that doesn't resolve to a section.
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .strict_at("nonexistent.section", false)
            .build_resolver()
            .err();
        match result {
            Some(ClapfigError::InvalidStrictPath { path, .. }) => {
                assert_eq!(path, "nonexistent.section");
            }
            other => panic!("expected InvalidStrictPath, got {other:?}"),
        }
    }

    #[test]
    fn strict_at_leaf_path_errors_at_build_resolver() {
        // host is a leaf, not a section.
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .strict_at("host", false)
            .build_resolver()
            .err();
        match result {
            Some(ClapfigError::InvalidStrictPath { path, reason }) => {
                assert_eq!(path, "host");
                assert!(reason.contains("leaf"));
            }
            other => panic!("expected InvalidStrictPath(leaf), got {other:?}"),
        }
    }

    #[test]
    fn strict_at_kebab_normalizes_then_rejects_unknown_path() {
        // `d-b` normalizes to `d_b`, which is not a section in the schema —
        // confirms normalization runs before lookup (otherwise the kebab
        // path would never reach the schema walker and we'd hit a
        // different error path).
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .normalize_keys(true)
            .strict_at("d-b", false)
            .build_resolver()
            .err();
        assert!(matches!(err, Some(ClapfigError::InvalidStrictPath { .. })));
    }

    #[test]
    fn strict_at_kebab_normalizes_to_real_snake_section() {
        // Success-path complement to the previous test: when the kebab
        // form actually corresponds to a real snake_case section in the
        // schema, build_resolver must accept it. Uses the kebab fixture
        // because its sole nested section is multi-word (`my_section`),
        // so the kebab → snake rewrite is observable.
        let result = Clapfig::builder(crate::fixtures::test::kebab_strict_at_schema())
            .app_name("demo")
            .normalize_keys(true)
            .strict_at("my-section", false)
            .build_resolver();
        assert!(
            result.is_ok(),
            "kebab strict_at path resolving to a real snake section must build (got error: {:?})",
            result.err()
        );
    }

    // --- on_unknown_key basics ---

    #[test]
    fn on_unknown_key_accept_drops_silently() {
        // strict default rejects; callback Accepts → key drops, no error.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "rogue_key = 1\nport = 3000\n").unwrap();
        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .on_unknown_key(|_| crate::UnknownKeyDecision::Accept)
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(3000)));
    }

    #[test]
    fn on_unknown_key_reject_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "rogue_key = 1\n").unwrap();
        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .on_unknown_key(|_| crate::UnknownKeyDecision::Reject)
            .load();
        assert!(result.is_err());
    }

    #[test]
    fn on_unknown_key_context_carries_path_leaf_value_line() {
        // The callback must see path, leaf, value, file, and line.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "port = 3000\n# pad\nbogus = 42\n",
        )
        .unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(
            None::<(String, String, i64, Option<usize>)>,
        ));
        let seen_clone = std::sync::Arc::clone(&seen);
        let _result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .on_unknown_key(move |ctx: &crate::UnknownKeyContext<'_>| {
                if let Some(i) = ctx.value.and_then(|v| v.as_integer()) {
                    *seen_clone.lock().unwrap() =
                        Some((ctx.path.into(), ctx.leaf.into(), i, ctx.line));
                }
                crate::UnknownKeyDecision::Accept
            })
            .load();
        let captured = seen.lock().unwrap().clone();
        match captured {
            Some((path, leaf, value, line)) => {
                assert_eq!(path, "bogus");
                assert_eq!(leaf, "bogus");
                assert_eq!(value, 42);
                assert_eq!(line, Some(3));
            }
            None => panic!("callback never received the unknown key"),
        }
    }

    #[test]
    fn on_unknown_key_not_called_on_cascade_accepted_keys() {
        // strict_at("db", false) → unknown nested key drops without the
        // callback ever seeing it.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("demo.toml"),
            "[db]\nurl = \"pg://\"\nlenient_typo = 1\n",
        )
        .unwrap();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = std::sync::Arc::clone(&called);
        Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .strict_at("db", false)
            .on_unknown_key(move |_| {
                called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                crate::UnknownKeyDecision::Reject
            })
            .load()
            .unwrap();
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "callback must not run for cascade-accepted keys"
        );
    }

    // --- post_validate lifecycle ---

    #[test]
    fn post_validate_not_called_when_upstream_fails() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "typo_key = 1\n").unwrap();

        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .strict(true)
            .post_validate(move |_| {
                called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .load();

        assert!(result.is_err(), "strict validation should have failed");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "hook must not run when upstream resolution fails"
        );
    }

    // --- the builder file-name contract (value-model spec) ---

    #[test]
    fn file_stem_discovers_default_toml_only() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 4321\n").unwrap();
        // A same-stem file in a format that is NOT enabled is invisible
        // to discovery (formats are opt-in, never inferred).
        fs::write(dir.path().join("demo.json"), "{}").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(4321)));
    }

    #[test]
    fn file_stem_same_directory_multi_format_match_is_hard_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 1\n").unwrap();
        fs::write(dir.path().join("demo.json"), "{}").unwrap();

        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "json"])
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap_err();
        match err {
            ClapfigError::AmbiguousConfigFiles {
                dir: err_dir,
                files,
            } => {
                assert_eq!(err_dir, dir.path());
                assert_eq!(files.len(), 2);
                let msg = ClapfigError::AmbiguousConfigFiles {
                    dir: err_dir,
                    files,
                }
                .to_string();
                assert!(msg.contains("demo.toml"), "must name both files: {msg}");
                assert!(msg.contains("demo.json"), "must name both files: {msg}");
            }
            other => panic!("expected AmbiguousConfigFiles, got {other:?}"),
        }
    }

    #[test]
    fn file_stem_multi_format_enabled_single_match_loads() {
        // Enabling several formats is fine as long as each directory has
        // at most one same-stem file.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 7\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "json"])
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(7)));
    }

    #[test]
    fn file_stem_layering_across_directories_still_merges() {
        // Across directories the same-stem rule does NOT fire — each
        // directory contributes at most one file; normal layering applies.
        let low = TempDir::new().unwrap();
        let high = TempDir::new().unwrap();
        fs::write(low.path().join("demo.toml"), "port = 1\nhost = \"low\"\n").unwrap();
        fs::write(high.path().join("demo.toml"), "port = 2\n").unwrap();

        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .search_paths(vec![
                SearchPath::Path(low.path().to_path_buf()),
                SearchPath::Path(high.path().to_path_buf()),
            ])
            .no_env()
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(2)));
        assert_eq!(table.get("host"), Some(&Value::String("low".into())));
    }

    #[test]
    fn formats_unknown_name_errors() {
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "xml"])
            .no_env()
            .build_resolver()
            .err();
        match err {
            Some(ClapfigError::UnknownFormat { name, available }) => {
                assert_eq!(name, "xml");
                assert_eq!(available, ["toml", "yaml", "json"]);
            }
            other => panic!("expected UnknownFormat, got {other:?}"),
        }
    }

    #[test]
    fn file_stem_set_creates_preferred_format_file_seeded_from_template() {
        // `config set` against a stem scope with no existing file creates
        // `<stem>.<preferred extension>` seeded from the template.
        let dir = TempDir::new().unwrap();
        Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "12345".into(),
                scope: None,
            })
            .unwrap();
        let created = dir.path().join("demo.toml");
        assert!(created.exists(), "preferred-format file must be created");
        let content = fs::read_to_string(&created).unwrap();
        assert!(content.contains("port = 12345"));
        // Seeded from the template → doc comments present.
        assert!(content.contains("# App host"));
    }

    #[test]
    fn file_stem_set_edits_the_single_existing_same_stem_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 1\n").unwrap();
        Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "json"])
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "2".into(),
                scope: None,
            })
            .unwrap();
        let content = fs::read_to_string(dir.path().join("demo.toml")).unwrap();
        assert!(content.contains("port = 2"));
        assert!(
            !dir.path().join("demo.json").exists(),
            "set must edit the existing file in its own format"
        );
    }

    #[test]
    fn file_stem_set_with_ambiguous_files_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demo.toml"), "port = 1\n").unwrap();
        fs::write(dir.path().join("demo.json"), "{}").unwrap();
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "json"])
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "2".into(),
                scope: None,
            })
            .unwrap_err();
        assert!(matches!(err, ClapfigError::AmbiguousConfigFiles { .. }));
    }

    #[test]
    fn exact_file_name_without_extension_still_parses_as_toml() {
        // Behavior preservation: extensionless exact names (e.g. an rc
        // file) keep parsing as TOML, exactly as before the registry.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("demorc"), "port = 999\n").unwrap();
        let table = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demorc")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        assert_eq!(table.get("port"), Some(&Value::Integer(999)));
    }

    #[test]
    fn exact_file_name_with_unknown_extension_errors() {
        // An extension no adapter claims is a hard error at
        // build/load time — never a silent parse-as-TOML fallback.
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.ini")
            .no_env()
            .build_resolver()
            .err();
        match err {
            Some(ClapfigError::UnknownFormat { name, available }) => {
                assert_eq!(name, "ini");
                assert_eq!(available, ["toml", "yaml", "json"]);
            }
            other => panic!("expected UnknownFormat, got {other:?}"),
        }
    }

    #[test]
    fn formats_empty_list_errors() {
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(Vec::<String>::new())
            .no_env()
            .build_resolver()
            .err();
        match err {
            Some(ClapfigError::InvalidFormats { reason }) => {
                assert!(reason.contains("at least one"), "got: {reason}");
            }
            other => panic!("expected InvalidFormats, got {other:?}"),
        }
    }

    #[test]
    fn formats_duplicate_name_errors() {
        // A repeated name would register the same extension twice, so
        // stem discovery would collect one physical file twice and
        // misreport it as AmbiguousConfigFiles.
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_stem("demo")
            .formats(["toml", "toml"])
            .no_env()
            .build_resolver()
            .err();
        match err {
            Some(ClapfigError::InvalidFormats { reason }) => {
                assert!(reason.contains("'toml'"), "got: {reason}");
            }
            other => panic!("expected InvalidFormats, got {other:?}"),
        }
    }

    #[test]
    fn gen_output_unknown_extension_errors() {
        // `gen --output config.ini` must not silently write the
        // preferred format's content under a foreign extension.
        let dir = TempDir::new().unwrap();
        let out_path = dir.path().join("config.ini");
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .no_env()
            .handle(&ConfigAction::Gen {
                output: Some(out_path.clone()),
            })
            .unwrap_err();
        assert!(
            matches!(err, ClapfigError::UnknownFormat { ref name, .. } if name == "ini"),
            "expected UnknownFormat('ini'), got {err:?}"
        );
        assert!(!out_path.exists(), "no file may be written on error");
    }

    #[test]
    fn set_explicit_path_unknown_extension_errors() {
        // Persist targets follow the same explicit-path rule: an
        // unclaimed extension is a hard error, not TOML-under-.ini.
        let dir = TempDir::new().unwrap();
        let err = Clapfig::builder(demo_schema())
            .app_name("demo")
            .file_name("demo.ini")
            .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
            .no_env()
            .handle(&ConfigAction::Set {
                key: "port".into(),
                value: "1".into(),
                scope: None,
            })
            .unwrap_err();
        assert!(
            matches!(err, ClapfigError::UnknownFormat { ref name, .. } if name == "ini"),
            "expected UnknownFormat('ini'), got {err:?}"
        );
    }

    // --- schema-driven datetime coercion, end to end ---

    #[test]
    fn override_string_coerces_into_datetime_leaf() {
        // CLI/URL/env layers are schema-blind (heuristics deliver
        // strings); the DateTime leaf declaration coerces at finalize
        // (ADR-0001).
        let dir = TempDir::new().unwrap();
        let schema = Schema::object("T")
            .field("stamp", RtField::datetime().optional())
            .build();
        let table = Clapfig::builder(schema)
            .app_name("demo")
            .file_name("demo.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .cli_override("stamp", Some("1979-05-27T07:32:00Z"))
            .load()
            .unwrap();
        match &table["stamp"] {
            Value::Datetime(dt) => assert_eq!(dt.to_string(), "1979-05-27T07:32:00Z"),
            other => panic!("expected Datetime, got {other:?}"),
        }
    }

    #[test]
    fn post_validate_second_call_replaces_first() {
        let dir = TempDir::new().unwrap();

        let result = Clapfig::builder(demo_schema())
            .app_name("demo")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .post_validate(|_| Err("first".into()))
            .post_validate(|_| Err("second".into()))
            .load();

        match result {
            Err(ClapfigError::PostValidationFailed(msg)) => assert_eq!(msg, "second"),
            other => panic!("expected PostValidationFailed('second'), got {other:?}"),
        }
    }
}
