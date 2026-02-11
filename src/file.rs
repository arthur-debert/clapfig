use std::path::PathBuf;

use crate::error::ClapfigError;
use crate::types::SearchPath;

/// Resolve a `SearchPath` to a concrete directory path.
///
/// `app_name` is used by `SearchPath::Platform` to construct the platform-specific
/// config directory (e.g. `~/.config/{app_name}/` on Linux).
///
/// Returns `None` if the path cannot be resolved (e.g. no home directory found).
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
    }
}

/// For each search path, check if `{dir}/{file_name}` exists and read its content.
///
/// Returns `(PathBuf, String)` pairs in search-path order (first = lowest priority).
/// Missing files are silently skipped.
pub fn load_config_files(
    search_paths: &[SearchPath],
    file_name: &str,
    app_name: &str,
) -> Result<Vec<(PathBuf, String)>, ClapfigError> {
    let mut results = Vec::new();

    for sp in search_paths {
        let Some(dir) = resolve_search_path(sp, app_name) else {
            continue;
        };
        let file_path = dir.join(file_name);
        match std::fs::read_to_string(&file_path) {
            Ok(content) => results.push((file_path, content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ClapfigError::IoError {
                    path: file_path,
                    source: e,
                })
            }
        }
    }

    Ok(results)
}

/// Determine the primary config file path (first resolved search path).
/// Used as the target for `config set` persistence.
pub fn primary_config_path(
    search_paths: &[SearchPath],
    file_name: &str,
    app_name: &str,
) -> Option<PathBuf> {
    search_paths
        .iter()
        .find_map(|sp| resolve_search_path(sp, app_name))
        .map(|dir| dir.join(file_name))
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

    #[test]
    fn load_no_files_exist() {
        let dir = TempDir::new().unwrap();
        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let files = load_config_files(&paths, "nonexistent.toml", "test").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn load_one_file_exists() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.toml"), "port = 3000\n").unwrap();
        let paths = vec![SearchPath::Path(dir.path().to_path_buf())];
        let files = load_config_files(&paths, "app.toml", "test").unwrap();
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
        let files = load_config_files(&paths, "app.toml", "test").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].1.contains("host"));
        assert!(files[1].1.contains("port"));
    }

    #[test]
    fn missing_file_silently_skipped() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        // Only dir2 has a file
        fs::write(dir2.path().join("app.toml"), "port = 1\n").unwrap();

        let paths = vec![
            SearchPath::Path(dir1.path().to_path_buf()),
            SearchPath::Path(dir2.path().to_path_buf()),
        ];
        let files = load_config_files(&paths, "app.toml", "test").unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn primary_config_path_uses_first_resolved() {
        let p1 = PathBuf::from("/first/dir");
        let p2 = PathBuf::from("/second/dir");
        let paths = vec![SearchPath::Path(p1.clone()), SearchPath::Path(p2)];
        let primary = primary_config_path(&paths, "app.toml", "test");
        assert_eq!(primary, Some(p1.join("app.toml")));
    }
}
