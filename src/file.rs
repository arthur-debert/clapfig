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
//! [`resolve_persist_path`] resolves the explicit [`SearchPath`] the user set via
//! `.persist_path()` on the builder. It rejects [`Ancestors`](SearchPath::Ancestors)
//! because that variant expands to multiple directories — a write target must be
//! unambiguous.

use std::path::PathBuf;

use crate::error::ClapfigError;
use crate::types::{Boundary, SearchMode, SearchPath};

/// Resolve a single-directory [`SearchPath`] to a concrete path.
///
/// `app_name` is used by `SearchPath::Platform` to construct the platform-specific
/// config directory (e.g. `~/.config/{app_name}/` on Linux).
///
/// Returns `None` if the path cannot be resolved (e.g. no home directory found).
///
/// # Panics
///
/// Panics if called with [`SearchPath::Ancestors`] — use [`expand_ancestors`] instead.
pub fn resolve_search_path(sp: &SearchPath, app_name: &str) -> Option<PathBuf> {
    match sp {
        SearchPath::Platform => {
            let proj = directories::ProjectDirs::from("", "", app_name)?;
            Some(proj.config_dir().to_path_buf())
        }
        SearchPath::Home(subdir) => {
            let user = directories::UserDirs::new()?;
            Some(user.home_dir().join(subdir))
        }
        SearchPath::Cwd => std::env::current_dir().ok(),
        SearchPath::Path(p) => Some(p.clone()),
        SearchPath::Ancestors(_) => {
            panic!("resolve_search_path called with Ancestors — use expand_ancestors instead")
        }
    }
}

/// Expand an [`Ancestors`](SearchPath::Ancestors) variant into concrete directories.
///
/// Walks from the current working directory toward the filesystem root, collecting
/// directories in **shallowest-first** order (root end first, CWD last). This
/// ensures the deepest directory has highest priority in the priority-ascending list.
///
/// The [`Boundary`] controls where the walk ends:
/// - [`Root`](Boundary::Root) — continues to the filesystem root.
/// - [`Marker(name)`](Boundary::Marker) — stops (inclusive) at the first directory
///   containing a file or subdirectory named `name`. Falls back to root if the
///   marker is never found.
pub fn expand_ancestors(boundary: &Boundary) -> Vec<PathBuf> {
    let Ok(cwd) = std::env::current_dir() else {
        return vec![];
    };
    expand_ancestors_from(cwd, boundary)
}

/// Like [`expand_ancestors`] but starting from an explicit directory instead of CWD.
///
/// Useful in tests and for callers that need to control the starting point.
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
/// Single-directory variants are resolved in place. `Ancestors` entries are expanded
/// inline via [`expand_ancestors`].
pub fn expand_search_paths(search_paths: &[SearchPath], app_name: &str) -> Vec<PathBuf> {
    expand_search_paths_from(search_paths, app_name, None)
}

/// Like [`expand_search_paths`] but with an optional explicit start directory for
/// `Ancestors` expansion (instead of CWD). Used in tests.
pub fn expand_search_paths_from(
    search_paths: &[SearchPath],
    app_name: &str,
    ancestors_start: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for sp in search_paths {
        match sp {
            SearchPath::Ancestors(boundary) => {
                let expanded = match ancestors_start {
                    Some(start) => expand_ancestors_from(start.to_path_buf(), boundary),
                    None => expand_ancestors(boundary),
                };
                dirs.extend(expanded);
            }
            other => {
                if let Some(dir) = resolve_search_path(other, app_name) {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

/// Load config files from the expanded directory list, respecting [`SearchMode`].
///
/// Directories are checked in order for `{dir}/{file_name}`. Missing files are
/// silently skipped; I/O errors are propagated.
///
/// - [`Merge`](SearchMode::Merge): returns all found files in priority order.
/// - [`FirstMatch`](SearchMode::FirstMatch): searches from highest priority (end)
///   and returns only the first file found.
pub fn load_config_files(
    search_paths: &[SearchPath],
    file_name: &str,
    app_name: &str,
    mode: SearchMode,
) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
    let dirs = expand_search_paths(search_paths, app_name);

    match mode {
        SearchMode::Merge => load_all(&dirs, file_name),
        SearchMode::FirstMatch => load_first_match(&dirs, file_name),
    }
}

/// Load all config files found across directories (for Merge mode).
fn load_all(dirs: &[PathBuf], file_name: &str) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
    let mut results = Vec::new();
    for dir in dirs {
        let file_path = dir.join(file_name);
        match std::fs::read_to_string(&file_path) {
            Ok(content) => results.push((file_path, content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ClapfigError::IoError {
                    path: file_path,
                    source: e,
                });
            }
        }
    }
    Ok(results)
}

/// Load only the highest-priority config file found (for FirstMatch mode).
///
/// Searches from the end of the directory list (highest priority) backward.
fn load_first_match(
    dirs: &[PathBuf],
    file_name: &str,
) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
    for dir in dirs.iter().rev() {
        let file_path = dir.join(file_name);
        match std::fs::read_to_string(&file_path) {
            Ok(content) => return Ok(vec![(file_path, content)]),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ClapfigError::IoError {
                    path: file_path,
                    source: e,
                });
            }
        }
    }
    Ok(vec![])
}

/// Resolve the persist path for `config set`.
///
/// Takes the explicit [`SearchPath`] the user configured via `.persist_path()`.
/// Returns an error if [`Ancestors`](SearchPath::Ancestors) is used (it resolves
/// to multiple directories and is not a valid write target).
pub fn resolve_persist_path(
    persist: &SearchPath,
    file_name: &str,
    app_name: &str,
) -> Result<PathBuf, ClapfigError> {
    match persist {
        SearchPath::Ancestors(_) => Err(ClapfigError::AncestorsNotAllowedAsPersistPath),
        other => resolve_search_path(other, app_name)
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
        let resolved = resolve_search_path(&SearchPath::Path(p.clone()), "ignored");
        assert_eq!(resolved, Some(p));
    }

    // --- load_config_files tests (Merge mode, backward compat) ---

    #[test]
    fn load_no_files_exist() {
        let dir = TempDir::new().unwrap();
        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let files =
            load_config_files(&paths, "nonexistent.toml", "test", SearchMode::Merge).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn load_one_file_exists() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.toml"), "port = 3000\n").unwrap();
        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let files = load_config_files(&paths, "app.toml", "test", SearchMode::Merge).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "port = 3000\n");
    }

    #[test]
    fn load_multiple_files() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir1.path().join("app.toml"), "host = \"a\"\n").unwrap();
        fs::write(dir2.path().join("app.toml"), "port = 1000\n").unwrap();

        let paths = vec![
            SearchPath::Path(dir1.path().to_path_buf()),
            SearchPath::Path(dir2.path().to_path_buf()),
        ];
        let files = load_config_files(&paths, "app.toml", "test", SearchMode::Merge).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].1.contains("host"));
        assert!(files[1].1.contains("port"));
    }

    #[test]
    fn missing_file_silently_skipped() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir2.path().join("app.toml"), "port = 1\n").unwrap();

        let paths = vec![
            SearchPath::Path(dir1.path().to_path_buf()),
            SearchPath::Path(dir2.path().to_path_buf()),
        ];
        let files = load_config_files(&paths, "app.toml", "test", SearchMode::Merge).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_returns_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("app.toml");
        fs::write(&file_path, "port = 1\n").unwrap();
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o000)).unwrap();

        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let result = load_config_files(&paths, "app.toml", "test", SearchMode::Merge);
        assert!(result.is_err());

        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // --- FirstMatch mode ---

    #[test]
    fn first_match_returns_highest_priority() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        fs::write(dir1.path().join("app.toml"), "host = \"low\"\n").unwrap();
        fs::write(dir2.path().join("app.toml"), "host = \"high\"\n").unwrap();

        let paths = vec![
            SearchPath::Path(dir1.path().to_path_buf()),
            SearchPath::Path(dir2.path().to_path_buf()), // highest priority
        ];
        let files = load_config_files(&paths, "app.toml", "test", SearchMode::FirstMatch).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].1.contains("high"));
    }

    #[test]
    fn first_match_falls_back_to_lower_priority() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        // Only dir1 (lower priority) has a file
        fs::write(dir1.path().join("app.toml"), "host = \"fallback\"\n").unwrap();

        let paths = vec![
            SearchPath::Path(dir1.path().to_path_buf()),
            SearchPath::Path(dir2.path().to_path_buf()),
        ];
        let files = load_config_files(&paths, "app.toml", "test", SearchMode::FirstMatch).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].1.contains("fallback"));
    }

    #[test]
    fn first_match_returns_empty_when_no_files() {
        let dir = TempDir::new().unwrap();
        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let files =
            load_config_files(&paths, "nonexistent.toml", "test", SearchMode::FirstMatch).unwrap();
        assert!(files.is_empty());
    }

    // --- Ancestors expansion ---

    #[test]
    fn expand_ancestors_root_includes_cwd() {
        let dirs = expand_ancestors(&Boundary::Root);
        assert!(!dirs.is_empty());
        // Last entry should be CWD (highest priority)
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(dirs.last().unwrap(), &cwd);
    }

    #[test]
    fn expand_ancestors_root_is_shallowest_first() {
        let dirs = expand_ancestors(&Boundary::Root);
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

        // Build a path list mixing an explicit path with ancestors
        // We test via expand_search_paths_from to control the CWD
        let paths = vec![
            SearchPath::Path(explicit.path().to_path_buf()),
            SearchPath::Ancestors(Boundary::Marker(".marker")),
        ];

        let dirs = expand_search_paths_from(&paths, "test", Some(&deep));

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

    // --- Ancestors + FirstMatch integration ---

    #[test]
    fn ancestors_first_match_finds_nearest() {
        let root = TempDir::new().unwrap();
        let mid = root.path().join("mid");
        let deep = mid.join("deep");
        fs::create_dir_all(&deep).unwrap();

        // Config at mid level only
        fs::write(mid.join("app.toml"), "host = \"mid\"\n").unwrap();
        // Config at root level
        fs::write(root.path().join("app.toml"), "host = \"root\"\n").unwrap();

        // Simulate ancestors from deep: [root, mid, deep] in priority order
        let dirs = vec![root.path().to_path_buf(), mid.clone(), deep.clone()];

        let files = load_first_match(&dirs, "app.toml").unwrap();
        assert_eq!(files.len(), 1);
        // Should find mid (not root), since deep has no file and mid is next-highest priority
        assert!(files[0].1.contains("mid"));
    }

    #[test]
    fn ancestors_merge_layers_all() {
        let root = TempDir::new().unwrap();
        let mid = root.path().join("mid");
        let deep = mid.join("deep");
        fs::create_dir_all(&deep).unwrap();

        fs::write(root.path().join("app.toml"), "host = \"root\"\n").unwrap();
        fs::write(mid.join("app.toml"), "port = 9000\n").unwrap();

        let dirs = vec![root.path().to_path_buf(), mid.clone(), deep.clone()];

        let files = load_all(&dirs, "app.toml").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].1.contains("root")); // lower priority
        assert!(files[1].1.contains("9000")); // higher priority
    }
}
