# Runtime-defined schemas and cascading strictness

## Motivation

Clapfig today is built around a single source of truth: a Rust struct that
derives `confique::Config`. The struct's compile-time `META` tree drives every
operation — discovery, merging, validation, template generation, key lookup,
persistence. This works beautifully when the config shape is known at build
time, but it shuts out two real use cases:

1. **Apps whose config shape is not known at compile time.** Plugin hosts,
   scripting environments, web tools that build a settings UI from a
   user-supplied schema, services that ingest third-party config descriptions.
   These callers have a schema — just not one they can express as a Rust
   `struct` derive.

2. **Apps with mixed-strictness configs.** Today `.strict(true)` is a single
   global flag: either every unknown key is an error, or none of them are.
   That's fine for a small, fully-known config. It breaks down once part of
   the tree is a free-form bag — plugin-specific settings, user-defined
   sections, vendor extensions — that legitimately accepts keys clapfig
   cannot know about. The current escape hatch (turn strict off entirely)
   loses typo protection on the rest of the config.

Both gaps share a root cause: the schema is hard-coded as the static
`confique::meta::Meta` tree, and strictness is a single bit attached to the
whole resolution. This proposal generalizes both.

## Use cases

**Runtime schemas.**

- A static site generator that lets users describe their front-matter shape
  in a JSON Schema file, then validates every page's front-matter against
  it — using clapfig's layered-config machinery (defaults, env, per-directory
  overrides) for free.
- A WASM frontend that fetches a settings schema from the server and renders
  a form whose values are then merged through clapfig's URL-query / local-
  storage / defaults pipeline.
- A plugin system where each plugin contributes its own config sub-tree at
  load time. The host doesn't know plugin schemas at compile time but still
  wants merged, validated config delivered to each plugin.

**Cascading strictness.**

- A linter or build tool whose top-level config is strictly typed, but whose
  `[plugins.*]` section is a free-form map of plugin-specific settings.
  Today: choose between rejecting unknown plugin keys (breaks plugin
  authoring) or accepting unknown keys anywhere (silences typos in the
  typed part). With cascading strictness: `strict = true` everywhere except
  `plugins`, which is `strict = false` for its whole subtree.
- A monorepo tool whose project-level config is strict, but whose
  `[vendor]` block intentionally accepts whatever upstream tools write into
  it. The author wants typo protection on their own keys without policing
  vendor data.
- An app where a sub-config is itself a runtime schema (point 1) contributed
  by a third party. The parent doesn't want to dictate its strictness — the
  sub-config decides for itself.

## Specification

### Part 1 — Runtime-defined schemas

#### `Schema` and `Field`

A new owned-data analogue of `confique::meta::Meta`:

```rust
pub struct Schema {
    pub name: String,
    pub doc: Vec<String>,
    pub strict: Option<bool>,   // see Part 2
    pub fields: Vec<NamedField>,
}

pub struct NamedField {
    pub name: String,
    pub field: Field,
}

pub enum Field {
    Leaf {
        doc: Vec<String>,
        ty: LeafType,              // String | Integer | Float | Bool | Array | Map
        default: Option<toml::Value>,
        optional: bool,            // true => may be absent after merge
        env: Option<String>,       // optional env-var name override
    },
    Nested(Schema),
}
```

`Schema` is constructible directly (it's a plain data structure) and via a
fluent builder for readability:

```rust
let schema = Schema::object("AppConfig")
    .doc("Top-level application config")
    .field("host",     Field::string().doc("App host").default("localhost"))
    .field("port",     Field::integer().default(8080))
    .nested(
        Schema::object("Db")
            .field("url",       Field::string().optional())
            .field("pool_size", Field::integer().default(5))
    )
    .build();
```

`Schema` can also be loaded from a JSON Schema document — clapfig already
emits JSON Schema via `schema::generate_schema`; the inverse operation
closes the loop.

#### `Clapfig::runtime(schema)`

A new entry point parallel to `Clapfig::builder::<C>()`:

```rust
let cfg: toml::Table = Clapfig::runtime(schema)
    .app_name("myapp")
    .search_paths(vec![SearchPath::Cwd])
    .load()?;
```

The returned builder exposes the **same surface** as `ClapfigBuilder<C>` —
`app_name`, `file_name`, `search_paths`, `search_mode`, `persist_scope`,
`env_prefix`, `no_env`, `strict`, `normalize_keys`, `layer_order`,
`url_query`, `cli_override`, `cli_overrides_from`, `post_validate`,
`build_resolver`, `load`, `handle`. Behavior is identical except:

- The output type is `toml::Table`, not a typed `C`. (Callers who want JSON
  can `.to_json()` it; we surface a small helper for that.)
- `post_validate` receives `&toml::Table`, not `&C`.
- `cli_overrides_from(&S)` matches `S`'s serialized keys against the runtime
  schema's known fields, exactly mirroring the static case.

#### Internal abstraction: `ConfigSpec`

Internally we introduce a trait that both the static and the runtime path
implement. Everything that today consumes `C::META` (validation, template
generation, JSON Schema emission, `meta::doc_for`, `overrides::valid_keys`,
persistence) is rewritten to consume a borrowed `SchemaRef` view. The
static `confique::meta::Meta` is adapted into `SchemaRef` once at the entry
point. From the user's perspective this is invisible — it just makes both
paths share one implementation.

#### Validation, defaults, required fields

For runtime schemas clapfig owns the full pipeline (no confique on the
output side):

- **Unknown keys.** Walked against the `Schema` directly, using the same
  `find_key_line` source-text heuristic for error rendering.
- **Defaults.** Injected by recursively walking the `Schema` and filling
  any unset leaf whose `default` is `Some(_)`. Like confique, defaults are
  injected as-is (no `deserialize_with` equivalent in v1 — deferred).
- **Required fields.** After merge + defaults, any leaf with `optional: false`
  and no value is a `ClapfigError::MissingRequired { key }`. Same error
  pipeline as today.

#### What is intentionally not in v1

- **`deserialize_with`-style normalizers on runtime fields.** Confique's
  static path keeps working; the runtime path treats values as-is.
- **Custom enum types on runtime leaves.** Leaves are TOML primitives + array
  + map. A "constrained string" with an allowed-values list is a reasonable
  v2 addition.
- **Nested runtime schemas inside a static struct.** The two paths don't
  cross-pollinate in v1; the whole config is either static or runtime.

### Part 2 — Cascading strictness

#### The rule

For any unknown key at dotted path `a.b.c`, the **effective strictness** is
the value of `strict` on the nearest ancestor schema node (including the
key's parent struct itself) whose `strict` is explicitly set. If no ancestor
sets `strict`, the builder-level default applies.

That is the only rule. It produces both expected behaviors:

- A parent's `strict` value cascades to every descendant that does not
  itself set `strict`.
- The first descendant that sets its own `strict` becomes the new root for
  its subtree, overriding the inherited value below it.

#### Surface — runtime schemas

A schema node may set `strict`:

```rust
Schema::object("AppConfig")
    .strict(true)
    .nested(
        Schema::object("Plugins").strict(false)   // free-form subtree
    )
    .nested(
        Schema::object("Plugins.Audit").strict(true) // re-tightened
    );
```

#### Surface — static structs

`confique` is upstream and we don't extend its derive. Instead, the builder
takes an overlay:

```rust
Clapfig::builder::<AppConfig>()
    .app_name("myapp")
    .strict(true)                            // global default (unchanged)
    .strict_at("plugins", false)             // entire plugins.* subtree lenient
    .strict_at("plugins.audit", true)        // re-tighten one branch
    .load()?;
```

`strict_at(path, bool)` errors at build time if `path` does not resolve to
a known struct in the config schema (typo protection on the override
itself). When `normalize_keys(true)` is set, `path` may be written in
kebab-case (`pool-size`); it is normalized before lookup.

#### Surface — `strict(bool)` is the default

`ClapfigBuilder::strict(bool)` keeps its current meaning: it sets the
default that applies when no node-level override matches. Existing code
keeps working with no changes.

#### Behavior — what gets reported

`serde_ignored` (static path) and the schema walker (runtime path) both
produce a list of unknown-key dotted paths. Each path is filtered through
the strictness lookup; paths whose effective strictness is `false` are
dropped silently, paths whose effective strictness is `true` become
`ClapfigError::UnknownKeys` entries. Line numbers and source snippets are
unaffected.

#### Edge cases

- **`strict_at` on an `Option<Nested>` field.** Resolves normally; the
  override applies whether or not the section is present.
- **`strict_at` targets a leaf, not a struct.** Build-time error — strict
  is a property of containers.
- **Both runtime schema strict and builder `strict_at` on the same path
  (runtime path).** Builder overlay wins; it's the most local explicit
  statement.
- **Lenient subtree contains a value whose type is wrong.** Type errors
  still fire — strictness only governs unknown-key rejection, not type
  validation. A lenient subtree is "I don't know what keys exist here",
  not "I accept malformed values".

## How the user sees it

A user with a fully static config sees no change. Their existing
`.strict(true)` / `.strict(false)` calls behave identically.

A user with a static config and a free-form subtree adds one line:

```rust
.strict_at("plugins", false)
```

A user with no compile-time schema writes their schema as a `Schema` value
(or loads it from JSON Schema) and calls `Clapfig::runtime(schema)` instead
of `Clapfig::builder::<C>()`. The rest of their code — search paths, env
vars, persistence, post-validation — looks the same. Output is a
`toml::Table` instead of a typed struct.

A user mixing both — runtime schema with strict-per-section semantics —
sets `strict` on each `Schema::object` as they build it; the cascade
handles the rest.

## Compatibility

No existing public API changes. `Clapfig::builder::<C>()` keeps its current
signature and behavior. New surface is purely additive:

- `Schema`, `Field`, `Clapfig::runtime(schema)`
- `ClapfigBuilder::strict_at(path, bool)`
- `Schema::strict(bool)` (only meaningful on runtime schemas)

The internal refactor that introduces `ConfigSpec` / `SchemaRef` is not
visible to users.
