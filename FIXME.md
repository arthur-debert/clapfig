# Clapfig v1.0 Review

## Issue Tracker

| Category | # | Title | Fixed in commit |
|----------|---|-------|-----------------|
| Correctness | 1 | `add_search_path` doesn't preserve default `Platform` path | |
| Correctness | 2 | Silent data loss in `env.rs` when flat key conflicts with nested key | |
| Correctness | 3 | `find_key_line` fragile with duplicate leaf names across sections | |
| Correctness | 4 | Unreachable trailing `None` in `table_get` | |
| Completeness | 5 | `config gen -o` flag parsed but never honored | |
| Completeness | 6 | `Format` enum is dead code | |
| Code Quality | 7 | `merge` module is `pub` with no reason to be | |
| Code Quality | 8 | Wasteful serialize round-trip in `resolve.rs` | |
| Test Quality | 9 | `types.rs` tests assert nothing meaningful | |
| Test Quality | 10 | Missing test: `add_search_path` appending to already-set list | |
| Test Quality | 11 | Missing test: `find_key_line` edge cases | |
| Test Quality | 12 | Missing test: env var type conflict (flat vs nested) | |
| Test Quality | 13 | Missing test: I/O permission errors | |
| Documentation | 14 | No crate-level doc comment in `lib.rs` | |
| Documentation | 15 | Internal modules lack `//!` doc comments | |
| Documentation | 16 | `find_key_line` should document its heuristic nature | |

## Detailed Findings

### Correctness

#### #1 — `add_search_path` doesn't preserve default `Platform` path

`builder.rs:70-75`: When called without a prior `.search_paths()`, `add_search_path` uses
`get_or_insert_with(Vec::new)` — creating an empty vec, not `[Platform]`. The README describes
it as "append a path without replacing", but calling `.add_search_path(Cwd)` yields `[Cwd]`
instead of the expected `[Platform, Cwd]`. The test at `builder.rs:253` confirms the broken
behavior.

#### #2 — Silent data loss in `env.rs` when flat key conflicts with nested key

`env.rs:42-48`: If `MYAPP__DATABASE=flat` is processed before `MYAPP__DATABASE__URL=pg://`,
the first sets `database` to a string. The second tries `if let Value::Table(sub_table) = sub`
which silently fails, dropping the URL value. Env var iteration order is not guaranteed.

#### #3 — `find_key_line` fragile with duplicate leaf names across sections

`validate.rs:53-66`: Searches for the leaf key name by scanning lines from the top.
If two TOML sections have the same field name, it always returns the first match regardless
of which section the unknown key is actually in. Also doesn't handle quoted TOML keys.

#### #4 — Unreachable trailing `None` in `table_get`

`ops.rs:58-69`: The loop always returns via the `i == segments.len() - 1` check before
reaching the final `None`. Not a bug, just dead code that makes the logic harder to read.

### Completeness

#### #5 — `config gen -o` flag parsed but never honored

The `-o` flag is parsed by clap (`cli.rs:35`) and carried in `ConfigAction::Gen { output }`,
but the handler in `builder.rs:178` uses `Gen { .. }` — destructuring away `output` without
using it. The README (line 246) advertises `myapp config gen -o myapp.toml`.

#### #6 — `Format` enum is dead code

`types.rs:17-21` defines `Format::Toml` and it's exported from `lib.rs`, but nothing in the
codebase references it. Premature abstraction for hypothetical future format support.

### Code Quality

#### #7 — `merge` module is `pub` with no reason to be

`lib.rs:4`: `pub mod merge` exposes `deep_merge` as public API, but it operates on
`toml::Table` internals and isn't documented as part of the intended API surface.
Should be `pub(crate)`.

#### #8 — Wasteful serialize round-trip in `resolve.rs`

`resolve.rs:67-75`: The merged `toml::Table` is serialized to a string via `toml::to_string`
then re-parsed via `toml::from_str` to get `C::Layer`. This round-trip is unnecessary —
the `Table` can be deserialized directly using `toml::Value::try_into()` or
`C::Layer::deserialize()`.

### Test Quality

#### #9 — `types.rs` tests assert nothing meaningful

The tests in `types.rs:33-59` just construct enum variants without asserting any behavior.
They provide zero signal — if a variant compiles, the test passes, which the compiler already
guarantees.

#### #10 — Missing test: `add_search_path` appending to already-set list

No test verifies `.search_paths(vec![Platform]).add_search_path(Cwd)` yields `[Platform, Cwd]`.

#### #11 — Missing test: `find_key_line` edge cases

No tests for: duplicate leaf names in different sections, quoted TOML keys, or inline tables.

#### #12 — Missing test: env var type conflict (flat vs nested)

No test for what happens when `MYAPP__DATABASE=flat` and `MYAPP__DATABASE__URL=pg://` are both
present. The current behavior silently drops the nested var.

#### #13 — Missing test: I/O permission errors

No tests for `persist_value` or `load_config_files` encountering unreadable files or
permission-denied errors.

### Documentation

#### #14 — No crate-level doc comment in `lib.rs`

`lib.rs` has no `//!` block. This would be the first thing users see on docs.rs.

#### #15 — Internal modules lack `//!` doc comments

Modules like `validate`, `resolve`, `env`, `persist`, `merge`, `overrides`, and `file` have
no module-level doc comments explaining their role, design choices, or trade-offs.

#### #16 — `find_key_line` should document its heuristic nature

The function lacks a note about its limitations: it's a best-effort text search, not a proper
TOML parser, and can return wrong line numbers in edge cases.
