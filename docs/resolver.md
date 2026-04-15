# Resolver Guide

The `Resolver` is clapfig's answer to the `.editorconfig` / `.eslintrc` /
`.htaccess` pattern: tools that walk a file tree where every directory can
carry its own configuration, with ancestor configs layering in from above.

## The problem

`Clapfig::builder().load()` resolves config once, anchored at the process's
current working directory. For a simple CLI tool, that's exactly right. But
for tools that process many directories — static site generators, linters,
build systems — you need:

- **N resolutions from N directories**, each producing an independent config.
- **Ancestor merging**, where a leaf directory inherits settings from parent
  configs.
- **Amortized I/O**, so reading the same ancestor file across hundreds of
  leaves doesn't repeat disk reads.

## Building a Resolver

```rust
use clapfig::{Clapfig, Config, SearchPath, Boundary, SearchMode};

let resolver = Clapfig::builder::<SiteConfig>()
    .app_name("myssg")
    .file_name(".myssg.toml")
    .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
    .search_mode(SearchMode::Merge)
    .build_resolver()?;
```

`build_resolver()` captures the builder's state — search paths, env vars,
overrides, strict mode, post_validate hook — into a reusable handle. The
builder is consumed, just like `load()`.

## Resolving per directory

```rust
for leaf in walk_content_tree("./site") {
    let config = resolver.resolve_at(&leaf)?;
    render_page(&leaf, &config);
}
```

Each `resolve_at()` call is a fully independent resolution:

- **`SearchPath::Cwd`** resolves to the directory passed to `resolve_at()`,
  not the process CWD.
- **`SearchPath::Ancestors`** walks up from that directory.
- **All other layers** (env vars, CLI overrides, URL params) use the values
  captured at `build_resolver()` time.

## File caching

Files read during `resolve_at()` are cached by absolute path inside the
resolver. A tree walk that visits 1000 leaves sharing 5 ancestor config files
pays the disk+parse cost once per unique file, not 1000 times.

The cache lives for the lifetime of the `Resolver` instance. There is no
mtime-based invalidation — if files change on disk and you need freshness,
build a new `Resolver`. This is a deliberate simplicity choice.

```rust
// Cache is scoped to the resolver — drop it to invalidate
let resolver = make_resolver()?;
process_tree(&resolver);

// Need fresh data? Build a new one.
let resolver = make_resolver()?;
process_tree(&resolver);
```

## Search modes

The search mode controls what happens when multiple config files are found
during a walk:

### `SearchMode::Merge` (default)

Deep-merges all found files. Each file is a sparse overlay — later files
(closer to the leaf) override earlier ones key by key:

```
/project/.myssg.toml       → host = "base", port = 8080
/project/blog/.myssg.toml  → port = 3000
```

Resolving from `/project/blog/` produces `host = "base", port = 3000`.

### `SearchMode::FirstMatch`

Uses only the single nearest file found. Good when configs are self-contained
and should not layer:

```rust
let resolver = Clapfig::builder::<Config>()
    .app_name("fmt")
    .search_paths(vec![SearchPath::Ancestors(Boundary::Root)])
    .search_mode(SearchMode::FirstMatch)
    .build_resolver()?;
```

## Boundaries

The `Boundary` controls how far `SearchPath::Ancestors` walks:

- **`Boundary::Root`** — walks all the way to the filesystem root. Good for
  tools that should find configs anywhere above.
- **`Boundary::Marker(".git")`** — stops at the first directory containing
  `.git`. The marker directory is included in the search. Good for
  project-scoped tools.

```rust
// Walk up to the repo root
SearchPath::Ancestors(Boundary::Marker(".git"))

// Walk all the way up
SearchPath::Ancestors(Boundary::Root)
```

## Post-validate hook

A `post_validate` hook registered on the builder is captured into the resolver
and fires on every `resolve_at()` call — not just once. This means per-leaf
semantic validation works automatically:

```rust
let resolver = Clapfig::builder::<SiteConfig>()
    .app_name("myssg")
    .file_name(".myssg.toml")
    .search_paths(vec![SearchPath::Ancestors(Boundary::Marker(".git"))])
    .post_validate(|c| {
        if c.port < 1024 {
            return Err(format!("port {} is privileged", c.port));
        }
        Ok(())
    })
    .build_resolver()?;

// The hook runs on every resolve_at call
for leaf in leaves {
    let config = resolver.resolve_at(&leaf)?; // hook fires here
}
```

## Combining with fixed search paths

You can mix `Ancestors` with fixed search paths. Fixed paths (like `Platform`
or `Path`) contribute the same files to every `resolve_at()` call, while
`Ancestors` and `Cwd` vary per call:

```rust
let resolver = Clapfig::builder::<Config>()
    .app_name("mytool")
    .file_name(".mytool.toml")
    .search_paths(vec![
        SearchPath::Platform,                              // global defaults
        SearchPath::Ancestors(Boundary::Marker(".git")),   // project-local
        SearchPath::Cwd,                                   // leaf-local
    ])
    .build_resolver()?;
```

In merge mode, the platform config provides base defaults, ancestor configs
layer project-level overrides, and the leaf's own config has highest priority.

## Relationship to `load()`

`load()` is the special case. Internally it is equivalent to:

```rust
self.build_resolver()?.resolve_at(std::env::current_dir()?)
```

All resolution flows through one code path, so `load()` and `resolve_at()`
have identical merge semantics. If you start with `load()` and later need
multi-directory resolution, switching to `build_resolver()` is a mechanical
change — the config behavior stays the same.
