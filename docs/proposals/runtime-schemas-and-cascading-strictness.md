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
- A struct that mixes typed fields and a `#[serde(flatten)] BTreeMap`
  catch-all at the same level — typed-field typos should error, but keys
  intended for the catch-all (e.g. extension-emitted diagnostic codes
  containing a `.`) should land silently in the map. This is the
  [lex-fmt/lex use case driving issue #39][issue-39]: per-struct cascade
  alone is too coarse here (it can't tell `missing_footote` from
  `acme.task-due-date-missing` — both sit at the same struct level), so the
  proposal also includes a per-key callback that lets user code apply a
  domain-specific decision (e.g. "leaf contains a `.` → accept, otherwise
  reject").

[issue-39]: https://github.com/arthur-debert/clapfig/issues/39

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
        ty: LeafType,
        default: Option<toml::Value>,
        optional: bool,            // true => may be absent after merge
        env: Option<String>,       // optional env-var name override
    },
    Nested(Schema),
}

pub enum LeafType {
    String,
    Integer,
    Float,
    Bool,
    Array,
    Map,
    /// Constrained value: must equal one of the listed TOML values.
    /// Use for log levels, output formats, mode flags, etc.
    Enum { values: Vec<toml::Value> },
}
```

`Schema` is constructible directly (it's a plain data structure) and via a
fluent builder for readability:

```rust
let schema = Schema::object("AppConfig")
    .doc("Top-level application config")
    .field("host",     Field::string().doc("App host").default("localhost"))
    .field("port",     Field::integer().default(8080))
    .field("level",    Field::enum_of(["debug", "info", "warn", "error"])
                            .default("info")
                            .doc("Log verbosity"))
    .nested(
        Schema::object("Db")
            .doc("Database connection settings")
            .field("url",       Field::string().optional())
            .field("pool_size", Field::integer().default(5))
    )
    .build();
```

`doc(...)` on a `Schema` or `Field` is the runtime equivalent of a `///`
doc comment on a static struct field. Every consumer that reads doc
comments today — `config gen` template generation, `config get` output,
`schema` (JSON Schema) emission, `meta::doc_for` — reads from these
strings instead. There is no second-class status for runtime schemas: a
runtime-driven app can ship `myapp config gen` and produce a fully
commented template, including any enum's allowed values, with no extra
work.

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
- **Type and enum validation.** Each merged value is checked against the
  leaf's `LeafType`. For `LeafType::Enum { values }`, the merged value
  must equal one of `values` (TOML equality, with `normalize_keys`
  applied to string values when the option is on); a mismatch produces
  `ClapfigError::InvalidValue { key, reason }` — the same error variant
  static-path enum violations use today. `config set` consults the same
  check, so writing an out-of-set value fails fast before the file is
  touched (matching the behavior of `handle_set_rejects_invalid_enum_value`
  for static enums).

#### Enum leaves and `config gen`

`LeafType::Enum` is included in v1 because constrained value sets — log
levels, output formats, mode flags — are routine in real configs and
rolling them by hand in `post_validate` defeats the point of having a
schema. Static structs already get this via confique's enum derive; the
runtime path gets a parallel facility.

The generated template lists the allowed values inline so users do not
need to consult external docs:

```toml
# Log verbosity
# Allowed: "debug" | "info" | "warn" | "error"
level = "info"
```

The JSON Schema emitter renders the same set as a top-level `"enum": [...]`
on the property.

#### What is intentionally not in v1

- **`deserialize_with`-style normalizers on runtime fields.** Confique's
  static path keeps working; the runtime path treats values as-is.
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

#### Surface — per-key callback

The cascade rule is **per struct**, which is the right grain for cases like
"the whole `[plugins]` table is free-form" but is too coarse when typed
fields and a free-form catch-all share a struct level (`#[serde(flatten)]
BTreeMap` as a sibling of typed fields). For that case, the builder accepts
an optional callback that runs *after* cascade resolution decides a key
would be rejected:

```rust
pub struct UnknownKeyContext<'a> {
    /// Full dotted path with every segment unquoted, e.g.
    /// `diagnostics.rules.acme.task-due-date-missing`.
    pub path: &'a str,

    /// The single TOML key clapfig saw at the leaf position — i.e. the
    /// final element of the path *as TOML parsed it*, not the trailing
    /// piece of `path` split on `.`. A bare key like `missing_footote`
    /// gives `leaf = "missing_footote"`; a quoted key like
    /// `"acme.task-due-date-missing"` gives
    /// `leaf = "acme.task-due-date-missing"` (the dots are part of the
    /// key, not segment separators).
    ///
    /// This is the field the lex-fmt example below pattern-matches on:
    /// `leaf.contains('.')` distinguishes extension-emitted dotted keys
    /// from bare typo-like keys.
    pub leaf: &'a str,

    pub value: &'a toml::Value,
    pub file: Option<&'a Path>,
    pub line: Option<usize>,
}

pub enum UnknownKeyDecision {
    Reject,   // unchanged — produces ClapfigError::UnknownKeys
    Accept,   // silently dropped, same as a lenient subtree
}

ClapfigBuilder::on_unknown_key(
    impl Fn(&UnknownKeyContext) -> UnknownKeyDecision + Send + Sync + 'static
)
```

Same hook on the runtime builder (`Clapfig::runtime(schema).on_unknown_key(...)`).
Default is `Reject` — without the hook, behavior is identical to today.

**The decision chain:**

1. The schema walk (or `serde_ignored`) flags a key as unknown.
2. The strictness cascade resolves an effective strictness for that key.
3. If the cascade says **lenient**, drop silently. Done.
4. If the cascade says **strict** and a callback is registered, call it.
   `Accept` drops the key silently; `Reject` produces an
   `UnknownKeys` error.
5. If no callback is registered, the cascade decision stands.

So the callback never gates keys the cascade already accepts — it only
gives the user a last word on keys the cascade would have rejected. That
preserves the "no opt-in cost" property: code that doesn't register a
callback behaves exactly as before.

**Composition with cascade — the lex-fmt example:**

```rust
Clapfig::builder::<LexConfig>()
    .app_name("lex")
    .strict(true)                                  // typo protection everywhere
    .on_unknown_key(|c| {
        // Inside diagnostics.rules, dotted leaves are extension-emitted codes
        // (e.g. "acme.task-due-date-missing") and land in the flatten BTreeMap.
        // Bare leaves are typos of typed sibling fields — keep the error.
        if c.path.starts_with("diagnostics.rules.") && c.leaf.contains('.') {
            UnknownKeyDecision::Accept
        } else {
            UnknownKeyDecision::Reject
        }
    })
    .load()?;
```

This is the shape issue #39 needs. The cascade rule alone cannot express
it because typed and free-form keys are *siblings*, not parent and child.

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

A user with sibling-level catch-all needs (lex-fmt/#39) registers
`.on_unknown_key(|c| ...)`. The cascade alone won't do it; the callback
gives them a domain-specific last word.

The three knobs compose without leaking into each other:

| Knob | Grain | Default |
|------|-------|---------|
| `strict(bool)` | whole resolution | `true` |
| `strict_at(path, bool)` / `Schema::strict(bool)` | per struct subtree | inherit |
| `on_unknown_key(fn)` | per key | reject (no-op) |

## Compatibility

No existing public API changes. `Clapfig::builder::<C>()` keeps its current
signature and behavior. New surface is purely additive:

- `Schema`, `Field`, `Clapfig::runtime(schema)`
- `ClapfigBuilder::strict_at(path, bool)`
- `Schema::strict(bool)` (only meaningful on runtime schemas)
- `ClapfigBuilder::on_unknown_key(fn)` (also on runtime builder)
- `UnknownKeyContext`, `UnknownKeyDecision`

The internal refactor that introduces `ConfigSpec` / `SchemaRef` is not
visible to users.
