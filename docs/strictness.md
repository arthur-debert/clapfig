# Strict mode and the cascading strictness cascade

Strict mode is **on by default**. When a config file contains a key that
doesn't match any field in your schema, loading fails with the file
path, key name, and line number. This catches typos and stale keys
early.

Some apps want one uniform answer ("strict everywhere" or "lenient
everywhere"). Others want a mix: typed fields catch typos, a plugin
subtree passes unknown keys through, one branch inside that subtree
re-tightens. Clapfig models all of these with three composable knobs.

## The three knobs

### Knob 1 — `.strict(bool)`: the whole-resolution default

```rust,ignore
Clapfig::builder::<AppConfig>()
    .strict(true)   // unknown keys are errors (the default)
    .load()?;

Clapfig::builder::<AppConfig>()
    .strict(false)  // unknown keys are silently dropped
    .load()?;
```

Pre-existing knob. Applies to any unknown key whose ancestors don't
carry an explicit override.

### Knob 2 — per-section override

Two equivalent surfaces:

- `ClapfigBuilder::strict_at(path, bool)` — sets an override on a dotted
  path (validated against `C`'s schema).
- `RuntimeBuilder::strict_at(path, bool)` — same, validated against the
  runtime schema.
- `Schema::strict(bool)` — sets an override on a runtime schema node
  inline.

```rust,ignore
// Static path
Clapfig::builder::<AppConfig>()
    .strict_at("plugins", false)        // plugins.* subtree: lenient
    .strict_at("plugins.audit", true)   // …but plugins.audit re-tightens
    .load()?;

// Runtime path (inline)
let schema = Schema::object("App")
    .nested("plugins", Schema::object("Plugins").strict(false));
```

`path` must resolve to a nested-section node. Targeting a leaf or an
unknown path errors at `build_resolver()` time with
`ClapfigError::InvalidStrictPath`. With `.normalize_keys(true)` set,
`path` may be written in kebab-case.

### Knob 3 — `.on_unknown_key(callback)`: per-key last word

```rust,ignore
use clapfig::{UnknownKeyContext, UnknownKeyDecision};

Clapfig::builder::<AppConfig>()
    .strict(true)
    .on_unknown_key(|c: &UnknownKeyContext<'_>| {
        if c.leaf.contains('.') {
            UnknownKeyDecision::Accept   // extension-emitted dotted key
        } else {
            UnknownKeyDecision::Reject   // bare typo
        }
    })
    .load()?;
```

The callback runs **only on cascade-strict keys** — keys the cascade
already decided are lenient never reach it. It receives an
`UnknownKeyContext` with the dotted path, the raw TOML leaf key, the
parsed value as `Option<&toml::Value>` (`None` in the rare case lookup
can't resolve — out-of-bounds array index, path through a non-table
intermediate), the source file, and the 1-indexed line number, and
returns `Accept` (drop silently) or `Reject` (the default — error as
today).

## The cascade rule

> For any unknown key at dotted path `a.b.c`, the effective strictness
> is the value of the **nearest ancestor schema node** (including the
> key's parent) whose `strict` is explicitly set. If no ancestor
> override exists, the builder-level default applies.

That single rule produces both expected behaviors:

- A parent's `strict` cascades to every descendant that doesn't itself
  set `strict`.
- The first descendant that sets its own `strict` becomes the new root
  for its subtree, overriding the inherited value below it.

When both a `Schema::strict(...)` and a builder `strict_at(...)` target
the same path, the builder overlay wins — it's the most local explicit
statement.

## Decision chain on an unknown key

1. **Cascade:** walk from the key's section path upward, return the
   first explicit `strict` override found, else the builder default.
2. If the cascade says **lenient**, drop the key silently. Done.
3. If the cascade says **strict** and a callback is registered, call
   it; `Accept` drops silently, `Reject` errors.
4. If no callback is registered, error.

## Common patterns

### Typed fields + plugin catch-all subtree

```rust,ignore
.strict(true)                       // typo protection on typed fields
.strict_at("plugins", false)        // plugins.* is plugin-extension territory
```

### Re-tighten one branch of a lenient subtree

```rust,ignore
.strict_at("plugins", false)
.strict_at("plugins.audit", true)   // audit plugin must match its schema
```

### Sibling-level dotted-key catch-all (typed fields next to a `BTreeMap`)

The cascade alone can't tell apart "typed sibling" and "extension
sibling" — they're at the same node. The callback can:

```rust,ignore
.on_unknown_key(|c: &UnknownKeyContext<'_>| {
    if c.leaf.contains('.') {
        UnknownKeyDecision::Accept   // `"acme.task-due-date-missing"`
    } else {
        UnknownKeyDecision::Reject   // bare typo
    }
})
```

`UnknownKeyContext::leaf` is the raw TOML leaf key the parser saw,
which preserves quoted-key semantics — a literal
`"acme.task-due-date-missing"` stays as a single string with dots in
it, distinct from a dotted path `acme.task.due.date.missing` that's
four segments.

## Behavior compatibility note

Pre-Phase-3 `.strict(false)` skipped validation entirely, which had a
side effect of masking type errors in config files (they'd surface
later in confique's typed-deserialize step instead). Combining a
lenient default with at least one strict override
(`.strict(false).strict_at("X", true)`) now activates the validation
step, which can surface type errors that an unconditionally lenient
resolution would have masked. Plain `.strict(false)` with no
`strict_at(_, true)` is byte-identical to the old behavior.
