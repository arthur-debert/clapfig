//! Integration tests for the typed tree-walk path and the typed
//! collected-unknowns surfaces (DER01-WS06, #135):
//!
//! - `TypedBuilder::build_resolver` → `TypedResolver<C>`: per-call
//!   anchoring, typed output, file caching, and the typed
//!   `post_validate` hook firing on every `resolve_at`.
//! - `TypedBuilder::load_with_unknowns` and
//!   `TypedResolver::resolve_at_with_unknowns` routing
//!   `UnknownKeyDecision::Collect` keys to the caller.

#![cfg(feature = "derive")]

use clapfig::{Boundary, Clapfig, ClapfigError, Schema, SearchPath, UnknownKeyDecision};
use serde::{Deserialize, Serialize};
use std::fs;
use tempfile::TempDir;

#[derive(Schema, Serialize, Deserialize, Debug, PartialEq)]
struct SiteConfig {
    /// Page layout name.
    #[clapfig(default = "default")]
    layout: String,

    /// Whether drafts render.
    #[clapfig(default = false)]
    drafts: bool,
}

/// A three-level content tree where the root and one subdirectory carry
/// configs: `<root>/site.toml` (layout = "root") and
/// `<root>/blog/site.toml` (drafts = true). The `.marker` file bounds
/// the ancestor walk.
fn content_tree() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join(".marker"), "").unwrap();
    fs::write(root.path().join("site.toml"), "layout = \"root\"\n").unwrap();
    let blog = root.path().join("blog");
    let posts = blog.join("posts");
    fs::create_dir_all(&posts).unwrap();
    fs::write(blog.join("site.toml"), "drafts = true\n").unwrap();
    root
}

/// A marker-bounded ancestor-walking typed builder over `site.toml`.
fn site_builder() -> clapfig::TypedBuilder<SiteConfig> {
    Clapfig::typed::<SiteConfig>()
        .app_name("site")
        .file_name("site.toml")
        .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".marker"))])
        .no_env()
}

#[test]
fn typed_resolver_anchors_each_resolve_at_its_directory() {
    let root = content_tree();
    let resolver = site_builder().build_resolver().unwrap();

    // Root leaf: only the root config applies.
    let at_root: SiteConfig = resolver.resolve_at(root.path()).unwrap();
    assert_eq!(at_root.layout, "root");
    assert!(!at_root.drafts);

    // Deep leaf: root + blog configs merge, deeper wins per key.
    let at_posts: SiteConfig = resolver.resolve_at(root.path().join("blog/posts")).unwrap();
    assert_eq!(at_posts.layout, "root");
    assert!(at_posts.drafts);
}

#[test]
fn typed_resolver_caches_files_across_calls() {
    let root = content_tree();
    let resolver = site_builder().build_resolver().unwrap();
    resolver.resolve_at(root.path().join("blog/posts")).unwrap();
    let after_first = resolver.cache_size();
    assert_eq!(after_first, 2, "root + blog configs");
    resolver.resolve_at(root.path().join("blog")).unwrap();
    assert_eq!(resolver.cache_size(), after_first, "no re-read on a rewalk");
}

#[test]
fn typed_resolver_runs_typed_post_validate_on_every_resolve() {
    let root = content_tree();
    let resolver = site_builder()
        .post_validate(|c: &SiteConfig| {
            if c.drafts {
                return Err("drafts must stay off".into());
            }
            Ok(())
        })
        .build_resolver()
        .unwrap();

    // The root leaf passes the typed hook…
    assert!(resolver.resolve_at(root.path()).is_ok());
    // …and the blog leaf (drafts = true) is rejected by it.
    let err = resolver.resolve_at(root.path().join("blog")).unwrap_err();
    match err {
        ClapfigError::PostValidationFailed(msg) => assert!(msg.contains("drafts"), "{msg}"),
        other => panic!("expected PostValidationFailed, got {other:?}"),
    }
}

#[test]
fn typed_load_with_unknowns_routes_collected_keys() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("site.toml"),
        "layout = \"root\"\nextra_key = 42\n",
    )
    .unwrap();

    let (cfg, unknowns) = Clapfig::typed::<SiteConfig>()
        .app_name("site")
        .file_name("site.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .on_unknown_key(|_| UnknownKeyDecision::Collect)
        .load_with_unknowns()
        .unwrap();

    assert_eq!(cfg.layout, "root");
    assert_eq!(unknowns.len(), 1);
    assert_eq!(unknowns[0].path, "extra_key");
    assert_eq!(unknowns[0].value, Some(clapfig::value::Value::Integer(42)));
}

#[test]
fn typed_load_with_unknowns_is_empty_without_collectors() {
    let dir = TempDir::new().unwrap();
    let (cfg, unknowns) = Clapfig::typed::<SiteConfig>()
        .app_name("site")
        .file_name("site.toml")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load_with_unknowns()
        .unwrap();
    assert_eq!(cfg.layout, "default");
    assert!(unknowns.is_empty());
}

#[test]
fn typed_resolve_at_with_unknowns_routes_collected_keys() {
    let root = content_tree();
    fs::write(
        root.path().join("blog").join("site.toml"),
        "drafts = true\nextra_key = \"x\"\n",
    )
    .unwrap();

    let resolver = site_builder()
        .on_unknown_key(|_| UnknownKeyDecision::Collect)
        .build_resolver()
        .unwrap();

    // The clean root leaf collects nothing…
    let (at_root, unknowns) = resolver.resolve_at_with_unknowns(root.path()).unwrap();
    assert_eq!(at_root.layout, "root");
    assert!(unknowns.is_empty());

    // …and the blog leaf surfaces its unknown key alongside the typed
    // result.
    let (at_blog, unknowns) = resolver
        .resolve_at_with_unknowns(root.path().join("blog"))
        .unwrap();
    assert!(at_blog.drafts);
    assert_eq!(unknowns.len(), 1);
    assert_eq!(unknowns[0].path, "extra_key");
}
