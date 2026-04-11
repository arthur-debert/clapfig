//! File discovery and loading for config files.
//!
//! This module implements the **discovery** and **resolution** axes of clapfig's
//! config lookup (see [`types`](crate::types) for the full picture).
//!
//! # Discovery
//!
//! Each [`SearchPath`] variant is resolved to one or more concrete directories:
//!
//! - `Platform`, `Home`, `Cwd`, `Path` — resolve to a single directory.
//! - `Ancestors(boundary)` — expands inline into multiple directories by walking
//!   from the current working directory up toward the filesystem root. Directories
//!   are emitted **shallowest first** so that deeper (closer to CWD) directories
//!   have higher priority, matching the list convention of "last = highest priority."
//!
//! # Resolution
//!
//! After directories are expanded, each one is checked for `{dir}/{file_name}`:
//!
//! - [`SearchMode::Merge`] — all found files are returned in priority order. The
//!   caller (the resolve pipeline) deep-merges them so later files override earlier.
//! - [`SearchMode::FirstMatch`] — the list is searched from the **highest-priority
//!   end** and the first file found is returned as the sole result.
//!
//! Missing files are silently skipped in both modes. Only actual I/O errors
//! (permissions, etc.) are propagated.
//!
//! # Persistence
//!
//! [`resolve_persist_path`] resolves the [`SearchPath`] for a named persist scope.
//! It rejects [`Ancestors`](SearchPath::Ancestors) because that variant expands
//! to multiple directories — a write target must be unambiguous.

use std::path::{Path, PathBuf};

use crate::error::ClapfigError;
use crate::types::{Boundary, SearchPath};

/// Resolve a single-directory [`SearchPath`] to a concrete path.
///
/// `app_name` is used by `SearchPath::Platform` to construct the platform-specific
/// config directory (e.g. `~/.config/{app_name}/` on Linux).
///
/// `cwd_override` lets the caller interpret [`SearchPath::Cwd`] as an explicit
/// directory rather than the process's current working directory. This is used
/// by [`Resolver`](crate::Resolver) so that each `resolve_at(dir)` call treats
/// `dir` as its logical "current directory" — the key enabler for tree-walk
/// use cases where every leaf is its own resolution root.
///
/// Returns `None` if the path cannot be resolved (e.g. no home directory found).
///
/// # Panics
///
/// Panics if called with [`SearchPath::Ancestors`] — use [`expand_ancestors_from`] instead.
pub fn resolve_search_path(
    sp: &SearchPath,
    app_name: &str,
    cwd_override: Option<&Path>,
) -> Option<PathBuf> {
    match sp {
        SearchPath::Platform => {
            let proj = directories::ProjectDirs::from("", "", app_name)?;
            Some(proj.config_dir().to_path_buf())
        }
        SearchPath::Home(subdir) => {
            let user = directories::UserDirs::new()?;
            Some(user.home_dir().join(subdir))
        }
        SearchPath::Cwd => match cwd_override {
            Some(dir) => Some(dir.to_path_buf()),
            None => std::env::current_dir().ok(),
        },
        SearchPath::Path(p) => Some(p.clone()),
        SearchPath::Ancestors(_) => {
            panic!("resolve_search_path called with Ancestors — use expand_ancestors_from instead")
        }
    }
}

/// Expand an [`Ancestors`](SearchPath::Ancestors) variant into concrete directories,
/// starting from an explicit directory.
///
/// Walks from `start` toward the filesystem root, collecting directories in
/// **shallowest-first** order (root end first, `start` last). This ensures the
/// deepest directory has highest priority in the priority-ascending list.
///
/// The [`Boundary`] controls where the walk ends:
/// - [`Root`](Boundary::Root) — continues to the filesystem root.
/// - [`Marker(name)`](Boundary::Marker) — stops (inclusive) at the first directory
///   containing a file or subdirectory named `name`. Falls back to root if the
///   marker is never found.
pub fn expand_ancestors_from(start: PathBuf, boundary: &Boundary) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = start.as_path();

    loop {
        dirs.push(current.to_path_buf());

        if let Boundary::Marker(name) = boundary
            && current.join(name).exists()
        {
            break;
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break, // reached root
        }
    }

    // Reverse: shallowest first (lowest priority), deepest last (highest priority)
    dirs.reverse();
    dirs
}

/// Expand all search paths into a flat list of concrete directories (priority-ascending).
///
/// `start_dir` is the logical "current directory" used to interpret
/// [`SearchPath::Cwd`] and [`SearchPath::Ancestors`]. For top-level
/// [`load()`](crate::ClapfigBuilder::load) calls this is `std::env::current_dir()`;
/// for [`Resolver::resolve_at(dir)`](crate::Resolver::resolve_at) it is `dir`,
/// which lets tree-walk tools treat every leaf as its own resolution root.
pub fn expand_search_paths(
    search_paths: &[SearchPath],
    app_name: &str,
    start_dir: &Path,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for sp in search_paths {
        match sp {
            SearchPath::Ancestors(boundary) => {
                dirs.extend(expand_ancestors_from(start_dir.to_path_buf(), boundary));
            }
            other => {
                if let Some(dir) = resolve_search_path(other, app_name, Some(start_dir)) {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

/// Resolve the persist path for a named scope.
///
/// Takes the [`SearchPath`] from a persist scope.
/// Returns an error if [`Ancestors`](SearchPath::Ancestors) is used (it resolves
/// to multiple directories and is not a valid write target).
pub fn resolve_persist_path(
    persist: &SearchPath,
    file_name: &str,
    app_name: &str,
) -> Result<PathBuf, ClapfigError> {
    match persist {
        SearchPath::Ancestors(_) => Err(ClapfigError::AncestorsNotAllowedAsPersistPath),
        other => resolve_search_path(other, app_name, None)
            .map(|dir| dir.join(file_name))
            .ok_or(ClapfigError::NoPersistPath),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_explicit_path() {
        let p = PathBuf::from("/tmp/myapp");
        let resolved = resolve_search_path(&SearchPath::Path(p.clone()), "ignored", None);
        assert_eq!(resolved, Some(p));
    }

    #[test]
    fn resolve_cwd_uses_override_when_provided() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_search_path(&SearchPath::Cwd, "ignored", Some(tmp.path()));
        assert_eq!(resolved.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn resolve_cwd_falls_back_to_env_current_dir() {
        let resolved = resolve_search_path(&SearchPath::Cwd, "ignored", None);
        assert_eq!(resolved, std::env::current_dir().ok());
    }

    // --- Ancestors expansion ---

    #[test]
    fn expand_ancestors_root_includes_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let dirs = expand_ancestors_from(cwd.clone(), &Boundary::Root);
        assert!(!dirs.is_empty());
        // Last entry should be the start dir (highest priority)
        assert_eq!(dirs.last().unwrap(), &cwd);
    }

    #[test]
    fn expand_ancestors_root_is_shallowest_first() {
        let cwd = std::env::current_dir().unwrap();
        let dirs = expand_ancestors_from(cwd, &Boundary::Root);
        // First entry should be an ancestor of the last entry (or root)
        assert!(dirs.len() >= 2);
        // Each entry should be a parent of the next
        for pair in dirs.windows(2) {
            assert!(
                pair[1].starts_with(&pair[0]),
                "{:?} should start with {:?}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn expand_ancestors_marker_stops_at_marker() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        // Place marker at "a" level
        fs::create_dir(dir.path().join("a").join(".git")).unwrap();

        // We can't easily set CWD in a test, so test the walk logic directly
        // by verifying the expand_ancestors_from helper
        let dirs = expand_ancestors_from(deep.clone(), &Boundary::Marker(".git"));

        // Should include: a, a/b, a/b/c (stops at a which contains .git)
        // Should NOT include the temp dir root
        assert!(dirs.contains(&dir.path().join("a")));
        assert!(dirs.contains(&dir.path().join("a").join("b")));
        assert!(dirs.contains(&dir.path().join("a").join("b").join("c")));
        assert!(!dirs.contains(&dir.path().to_path_buf()));
    }

    #[test]
    fn expand_ancestors_marker_missing_walks_to_root() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("x").join("y");
        fs::create_dir_all(&deep).unwrap();

        let dirs = expand_ancestors_from(deep.clone(), &Boundary::Marker(".nonexistent"));

        // Should walk all the way — includes the temp root
        assert!(dirs.contains(&dir.path().to_path_buf()));
        assert!(dirs.contains(&deep));
    }

    // --- expand_search_paths ---

    #[test]
    fn expand_search_paths_mixes_single_and_ancestors() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir(dir.path().join("a").join(".marker")).unwrap();

        let explicit = TempDir::new().unwrap();

        // Build a path list mixing an explicit path with ancestors.
        // We pass `deep` as the start_dir so Ancestors walks up from there.
        let paths = vec![
            SearchPath::Path(explicit.path().to_path_buf()),
            SearchPath::Ancestors(Boundary::Marker(".marker")),
        ];

        let dirs = expand_search_paths(&paths, "test", &deep);

        // explicit dir should come first (lowest priority)
        assert_eq!(dirs[0], explicit.path().to_path_buf());
        // ancestors should follow: a (shallowest), a/b (deepest = highest priority)
        assert!(dirs.contains(&dir.path().join("a")));
        assert!(dirs.contains(&dir.path().join("a").join("b")));
        // a/b should come after a
        let pos_a = dirs
            .iter()
            .position(|d| d == &dir.path().join("a"))
            .unwrap();
        let pos_ab = dirs
            .iter()
            .position(|d| d == &dir.path().join("a").join("b"))
            .unwrap();
        assert!(pos_ab > pos_a);
    }

    // --- resolve_persist_path ---

    #[test]
    fn persist_path_explicit() {
        let p = PathBuf::from("/tmp/configs");
        let result = resolve_persist_path(&SearchPath::Path(p.clone()), "app.toml", "test");
        assert_eq!(result.unwrap(), p.join("app.toml"));
    }

    #[test]
    fn persist_path_rejects_ancestors() {
        let result =
            resolve_persist_path(&SearchPath::Ancestors(Boundary::Root), "app.toml", "test");
        assert!(matches!(
            result,
            Err(ClapfigError::AncestorsNotAllowedAsPersistPath)
        ));
    }

    // End-to-end "load files via Ancestors walk" coverage lives in
    // `resolver::tests` now that I/O happens through `Resolver::load_files_cached`.
}
