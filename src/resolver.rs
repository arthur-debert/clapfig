//! Reusable resolution handle for tree-walk use cases.
//!
//! [`Resolver<C>`] is built once from a [`ClapfigBuilder`] and can then be
//! called repeatedly with [`resolve_at(dir)`](Resolver::resolve_at) to produce
//! a typed configuration anchored at a specific directory. This unlocks the
//! `.htaccess` / `.gitignore` / `.editorconfig` pattern: a dynamic file tree
//! where every directory is its own resolution root, each leaf producing an
//! independently merged configuration.
//!
//! # Why a separate handle?
//!
//! Clapfig's original [`load()`](crate::ClapfigBuilder::load) consumes the builder
//! and anchors resolution at `std::env::current_dir()`. For single-invocation
//! CLI tools that's exactly right. But for tools that walk a content tree and
//! need per-directory config — static site generators, linters, format-as-you-go
//! editors — paying the full `load()` cost N times means re-reading the same
//! ancestor files on every leaf.
//!
//! `Resolver` captures the builder's configuration once, holds a parsed-file
//! cache, and lets the caller pass a different starting directory on each call.
//! [`SearchPath::Cwd`](crate::SearchPath::Cwd) and
//! [`SearchPath::Ancestors`](crate::SearchPath::Ancestors) are interpreted
//! relative to whatever directory was passed to `resolve_at`, so each call is
//! a fully independent resolution — same builder state, different starting
//! point.
//!
//! # File cache
//!
//! Files read during `resolve_at` are cached by absolute path inside the
//! `Resolver`. A tree walk that visits 1000 leaves sharing the same five
//! ancestor config files pays the disk+parse cost once per unique file, not
//! 1000×. The cache lives for the lifetime of the `Resolver` instance — if
//! underlying files can change on disk and the caller cares about freshness,
//! they should build a new `Resolver`. (The cache is not invalidated by
//! mtime checks; that is a deliberate "keep it simple for v1" choice.)
//!
//! # post_validate composition
//!
//! A [`post_validate`](crate::ClapfigBuilder::post_validate) hook registered on the
//! builder is captured into the `Resolver` at construction time and fires on
//! every `resolve_at` call — not just once. The same invariants the hook
//! enforced for `load()` still apply per leaf.
//!
//! # Example
//!
//! ```ignore
//! let resolver = Clapfig::builder::<MyConfig>()
//!     .app_name("myapp")
//!     .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
//!     .build_resolver()?;
//!
//! for leaf in walk_content_tree("./site") {
//!     let cfg = resolver.resolve_at(&leaf)?;
//!     render_page(&leaf, &cfg);
//! }
//! ```

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use confique::Config;
use serde::Deserialize;

use crate::builder::PostValidateHook;
use crate::error::ClapfigError;
use crate::file;
use crate::resolve::{self, ResolveInput};
use crate::types::{Layer, SearchMode, SearchPath};

/// A cached file's raw text. The parsed `toml::Table` is not cached separately
/// because `resolve::resolve` needs the raw text anyway (for strict-mode
/// snippet rendering and for confique's own parse path).
#[derive(Clone)]
struct CachedFile {
    contents: String,
}

/// Reusable configuration resolver for tree-walk use cases.
///
/// Built once via [`ClapfigBuilder::build_resolver`](crate::ClapfigBuilder::build_resolver), then called repeatedly
/// with [`resolve_at(dir)`](Resolver::resolve_at) to produce a typed
/// configuration anchored at a specific directory. See the
/// [crate-level "Tree-walk resolution" section](crate#tree-walk-resolution--the-resolverc-handle)
/// for the full design rationale.
pub struct Resolver<C: Config> {
    app_name: String,
    file_name: String,
    search_paths: Vec<SearchPath>,
    search_mode: SearchMode,
    env_prefix: Option<String>,
    env_enabled: bool,
    strict: bool,
    #[cfg(feature = "url")]
    url_overrides: Vec<(String, toml::Value)>,
    cli_overrides: Vec<(String, toml::Value)>,
    layer_order: Option<Vec<Layer>>,
    post_validate: Option<Arc<PostValidateHook<C>>>,
    file_cache: Mutex<HashMap<PathBuf, CachedFile>>,
    _phantom: PhantomData<C>,
}

impl<C: Config> Resolver<C> {
    /// Construct a resolver from a builder's captured state.
    ///
    /// Called by [`ClapfigBuilder::build_resolver`](crate::ClapfigBuilder::build_resolver)
    /// — not meant for direct use.
    /// The argument list is long but every entry is a direct forwarding of
    /// one builder field, so splitting into an intermediate struct would just
    /// move the same fields around without hiding any complexity.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_builder(
        app_name: String,
        file_name: String,
        search_paths: Vec<SearchPath>,
        search_mode: SearchMode,
        env_prefix: Option<String>,
        env_enabled: bool,
        strict: bool,
        #[cfg(feature = "url")] url_overrides: Vec<(String, toml::Value)>,
        cli_overrides: Vec<(String, toml::Value)>,
        layer_order: Option<Vec<Layer>>,
        post_validate: Option<PostValidateHook<C>>,
    ) -> Self {
        Self {
            app_name,
            file_name,
            search_paths,
            search_mode,
            env_prefix,
            env_enabled,
            strict,
            #[cfg(feature = "url")]
            url_overrides,
            cli_overrides,
            layer_order,
            post_validate: post_validate.map(Arc::new),
            file_cache: Mutex::new(HashMap::new()),
            _phantom: PhantomData,
        }
    }

    /// Resolve the configuration with `start_dir` as the logical "current
    /// directory".
    ///
    /// [`SearchPath::Cwd`](crate::SearchPath::Cwd) is interpreted as `start_dir`;
    /// [`SearchPath::Ancestors`](crate::SearchPath::Ancestors) walks up from
    /// `start_dir`. All other layers (env, URL, CLI) are applied identically
    /// to [`ClapfigBuilder::load`](crate::ClapfigBuilder::load), using the
    /// values captured at build time. If a
    /// [`post_validate`](crate::ClapfigBuilder::post_validate) hook was
    /// registered on the builder, it runs after the merge on every call.
    ///
    /// Files discovered during this call are cached by absolute path; a second
    /// `resolve_at` call that touches the same file will reuse the cached
    /// contents rather than re-reading from disk.
    pub fn resolve_at(&self, start_dir: impl AsRef<Path>) -> Result<C, ClapfigError>
    where
        C::Layer: for<'de> Deserialize<'de>,
    {
        let start_dir = start_dir.as_ref();
        let dirs = file::expand_search_paths(&self.search_paths, &self.app_name, start_dir);
        let files = self.load_files_cached(&dirs)?;
        let env_vars: Vec<(String, String)> = if self.env_enabled {
            std::env::vars().collect()
        } else {
            Vec::new()
        };

        let input = ResolveInput {
            files,
            env_vars,
            env_prefix: self.env_prefix.clone(),
            #[cfg(feature = "url")]
            url_overrides: self.url_overrides.clone(),
            cli_overrides: self.cli_overrides.clone(),
            strict: self.strict,
            layer_order: self.layer_order.clone(),
        };

        let config = resolve::resolve::<C>(input)?;
        if let Some(hook) = self.post_validate.as_ref() {
            hook(&config).map_err(ClapfigError::PostValidationFailed)?;
        }
        Ok(config)
    }

    /// Read config files from the resolved directory list, using the cache.
    ///
    /// Applies the configured [`SearchMode`] directly here so the cache can
    /// see which files a given call actually consumed. We intentionally do
    /// not delegate to [`file::load_config_files`] because that path bypasses
    /// the cache.
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

    /// Read a single file, consulting the cache. Returns `Ok(None)` for
    /// missing files (matching the existing "missing files are silently
    /// skipped" convention); other I/O errors propagate.
    fn read_cached(&self, path: &Path) -> Result<Option<String>, ClapfigError> {
        {
            let cache = self.file_cache.lock().expect("file_cache mutex poisoned");
            if let Some(cached) = cache.get(path) {
                return Ok(Some(cached.contents.clone()));
            }
        }

        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut cache = self.file_cache.lock().expect("file_cache mutex poisoned");
                cache.insert(
                    path.to_path_buf(),
                    CachedFile {
                        contents: contents.clone(),
                    },
                );
                Ok(Some(contents))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ClapfigError::IoError {
                path: path.to_path_buf(),
                source: e,
            }),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clapfig;
    use crate::fixtures::test::TestConfig;
    use crate::types::{Boundary, SearchMode};
    use std::fs;
    use tempfile::TempDir;

    /// Helper: build a resolver with no env vars and explicit search paths.
    fn resolver_with_paths(paths: Vec<SearchPath>) -> Resolver<TestConfig> {
        Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(paths)
            .no_env()
            .build_resolver()
            .unwrap()
    }

    // --- basic resolve_at round-trips ---

    #[test]
    fn resolve_at_reads_file_under_cwd_start_dir() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Cwd]);
        let config = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn resolve_at_different_dirs_produce_different_configs() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        fs::write(a.path().join("test.toml"), "port = 1111\n").unwrap();
        fs::write(b.path().join("test.toml"), "port = 2222\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Cwd]);
        let config_a = resolver.resolve_at(a.path()).unwrap();
        let config_b = resolver.resolve_at(b.path()).unwrap();

        assert_eq!(config_a.port, 1111);
        assert_eq!(config_b.port, 2222);
    }

    #[test]
    fn resolve_at_respects_defaults_when_no_file() {
        let dir = TempDir::new().unwrap();
        let resolver = resolver_with_paths(vec![SearchPath::Cwd]);
        let config = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(config.port, 8080); // default
        assert_eq!(config.host, "localhost");
    }

    // --- Ancestors walk from start_dir ---

    #[test]
    fn resolve_at_ancestors_walks_up_from_start_dir() {
        let root = TempDir::new().unwrap();
        let mid = root.path().join("mid");
        let deep = mid.join("deep");
        fs::create_dir_all(&deep).unwrap();

        // Root has a base config; mid overrides port; deep has no config of its own.
        fs::write(root.path().join("test.toml"), "host = \"rootish\"\n").unwrap();
        fs::write(mid.join("test.toml"), "port = 5555\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Ancestors(Boundary::Root)]);
        let config = resolver.resolve_at(&deep).unwrap();

        // Ancestors walk from deep hits root (lowest) and mid (higher); both layer in.
        assert_eq!(config.host, "rootish");
        assert_eq!(config.port, 5555);
    }

    #[test]
    fn resolve_at_ancestors_marker_stops_at_marker() {
        let root = TempDir::new().unwrap();
        let project = root.path().join("project");
        let leaf = project.join("sub").join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        fs::create_dir(project.join(".git")).unwrap();

        // Config at ROOT (outside the marker) should NOT be seen.
        fs::write(root.path().join("test.toml"), "port = 9999\n").unwrap();
        // Config at PROJECT (the marker directory) should be seen.
        fs::write(project.join("test.toml"), "port = 4444\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))]);
        let config = resolver.resolve_at(&leaf).unwrap();

        assert_eq!(config.port, 4444, "should find project/test.toml");
    }

    #[test]
    fn resolve_at_ancestors_each_leaf_independent() {
        let root = TempDir::new().unwrap();
        let a_leaf = root.path().join("a").join("leaf");
        let b_leaf = root.path().join("b").join("leaf");
        fs::create_dir_all(&a_leaf).unwrap();
        fs::create_dir_all(&b_leaf).unwrap();

        // Shared root config
        fs::write(root.path().join("test.toml"), "host = \"shared\"\n").unwrap();
        // Per-branch overrides
        fs::write(root.path().join("a").join("test.toml"), "port = 100\n").unwrap();
        fs::write(root.path().join("b").join("test.toml"), "port = 200\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Ancestors(Boundary::Root)]);
        let config_a = resolver.resolve_at(&a_leaf).unwrap();
        let config_b = resolver.resolve_at(&b_leaf).unwrap();

        assert_eq!(config_a.host, "shared");
        assert_eq!(config_b.host, "shared");
        assert_eq!(config_a.port, 100);
        assert_eq!(config_b.port, 200);
    }

    // --- file cache behavior ---

    #[test]
    fn cache_populates_on_first_read() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 3000\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Cwd]);
        assert_eq!(resolver.cache_size(), 0);

        resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(resolver.cache_size(), 1);
    }

    #[test]
    fn cache_hit_on_second_read_of_same_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.toml");
        fs::write(&path, "port = 3000\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Cwd]);

        // First call populates the cache.
        let config1 = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(config1.port, 3000);
        assert_eq!(resolver.cache_size(), 1);

        // Now rewrite the file on disk. If the cache is honored, the second
        // resolve should return the ORIGINAL value, not the new one.
        fs::write(&path, "port = 9999\n").unwrap();
        let config2 = resolver.resolve_at(dir.path()).unwrap();
        assert_eq!(
            config2.port, 3000,
            "cache should mask on-disk changes — build a new Resolver for freshness"
        );
        assert_eq!(resolver.cache_size(), 1, "no new cache entry");
    }

    #[test]
    fn cache_shared_ancestor_across_leaves_reads_once() {
        let root = TempDir::new().unwrap();
        let a_leaf = root.path().join("a");
        let b_leaf = root.path().join("b");
        fs::create_dir_all(&a_leaf).unwrap();
        fs::create_dir_all(&b_leaf).unwrap();

        // Only the shared root file exists — neither leaf has its own.
        fs::write(root.path().join("test.toml"), "port = 7777\n").unwrap();

        let resolver = resolver_with_paths(vec![SearchPath::Ancestors(Boundary::Root)]);

        let _ = resolver.resolve_at(&a_leaf).unwrap();
        let cache_after_a = resolver.cache_size();
        let _ = resolver.resolve_at(&b_leaf).unwrap();
        let cache_after_b = resolver.cache_size();

        // Both leaves share the root config file; the cache should NOT have
        // grown between calls for that shared file (it's the same absolute path).
        // We can't guarantee only ONE entry (different leaves walk different
        // dirs up to root) but the root file itself must be deduplicated.
        assert!(cache_after_a >= 1);
        // After resolving the second leaf, any ancestor file seen during the
        // first call is a cache hit — but new directories may still add
        // entries if they contain files. In this test only the root has a
        // file, so cache_after_b should equal cache_after_a.
        assert_eq!(
            cache_after_b, cache_after_a,
            "shared ancestor file should be deduplicated in cache"
        );
    }

    // --- post_validate hook composition ---

    #[test]
    fn post_validate_fires_on_every_resolve_at() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        fs::write(dir_a.path().join("test.toml"), "port = 3000\n").unwrap();
        fs::write(dir_b.path().join("test.toml"), "port = 4000\n").unwrap();

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let resolver = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Cwd])
            .no_env()
            .post_validate(move |_: &TestConfig| {
                call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .build_resolver()
            .unwrap();

        resolver.resolve_at(dir_a.path()).unwrap();
        resolver.resolve_at(dir_b.path()).unwrap();
        resolver.resolve_at(dir_a.path()).unwrap();

        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "hook must run once per resolve_at call"
        );
    }

    #[test]
    fn post_validate_rejection_propagates_from_resolve_at() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("test.toml"), "port = 80\n").unwrap();

        let resolver = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Cwd])
            .no_env()
            .post_validate(|c: &TestConfig| {
                if c.port < 1024 {
                    Err(format!("port {} is privileged", c.port))
                } else {
                    Ok(())
                }
            })
            .build_resolver()
            .unwrap();

        let result = resolver.resolve_at(dir.path());
        match result {
            Err(ClapfigError::PostValidationFailed(msg)) => {
                assert!(msg.contains("80"), "expected port in message: {msg}");
            }
            other => panic!("expected PostValidationFailed, got {other:?}"),
        }
    }

    // --- Merge mode + multiple files ---

    #[test]
    fn resolve_at_merge_layers_multiple_ancestors() {
        let root = TempDir::new().unwrap();
        let mid = root.path().join("mid");
        let leaf = mid.join("leaf");
        fs::create_dir_all(&leaf).unwrap();

        fs::write(root.path().join("test.toml"), "host = \"root\"\n").unwrap();
        fs::write(mid.join("test.toml"), "port = 1111\n").unwrap();
        fs::write(
            leaf.join("test.toml"),
            "host = \"leaf\"\n[database]\npool_size = 99\n",
        )
        .unwrap();

        let resolver = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Ancestors(Boundary::Root)])
            .search_mode(SearchMode::Merge)
            .no_env()
            .build_resolver()
            .unwrap();

        let config = resolver.resolve_at(&leaf).unwrap();
        assert_eq!(config.host, "leaf", "leaf (deepest) wins for host");
        assert_eq!(config.port, 1111, "mid contributes port");
        assert_eq!(
            config.database.pool_size, 99,
            "leaf contributes nested pool_size"
        );
    }

    #[test]
    fn resolve_at_first_match_picks_nearest_ancestor() {
        let root = TempDir::new().unwrap();
        let mid = root.path().join("mid");
        let leaf = mid.join("leaf");
        fs::create_dir_all(&leaf).unwrap();

        fs::write(root.path().join("test.toml"), "port = 1\n").unwrap();
        fs::write(mid.join("test.toml"), "port = 2\n").unwrap();
        // leaf has no file

        let resolver = Clapfig::builder::<TestConfig>()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Ancestors(Boundary::Root)])
            .search_mode(SearchMode::FirstMatch)
            .no_env()
            .build_resolver()
            .unwrap();

        let config = resolver.resolve_at(&leaf).unwrap();
        assert_eq!(config.port, 2, "nearest ancestor with a file wins");
    }

    // --- build_resolver error paths ---

    #[test]
    fn build_resolver_requires_app_name() {
        let result = Clapfig::builder::<TestConfig>().build_resolver();
        assert!(matches!(result, Err(ClapfigError::AppNameRequired)));
    }

    // --- load() still works through the Resolver path ---

    #[test]
    fn load_still_produces_same_config_as_before() {
        // Regression guard: load() is now
        // `build_resolver()?.resolve_at(env::current_dir()?)`. Defaults and
        // no-file-on-disk path must still yield the stock TestConfig.
        let dir = TempDir::new().unwrap();
        let config: TestConfig = Clapfig::builder()
            .app_name("test")
            .file_name("test.toml")
            .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
            .no_env()
            .load()
            .unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.host, "localhost");
        assert_eq!(config.database.pool_size, 5);
    }
}
