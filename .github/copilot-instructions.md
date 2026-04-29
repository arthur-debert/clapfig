# Copilot Instructions

This is a Rust project (CLI, library crate, or workspace).

## Before suggesting a fix

- Run the project's umbrella check script if one exists (in `scripts/`,
  commonly named `check`, `pre-commit`, `ci.sh`, or `rust-pre-commit` — run
  `ls scripts/` to see which); otherwise `cargo fmt --check && cargo clippy
  -- -D warnings && cargo test`. CI runs the same; suggestions that don't
  pass won't merge.
- Never propose changes that leave tests failing.
- Update the changelog's `Unreleased` section for user-visible changes
  (`CHANGELOG_UNRELEASED.md` if the project has one, otherwise the
  `## [Unreleased]` section of `CHANGELOG.md`).

## Style and scope

- Keep changes minimal. Don't add features, refactor, or introduce abstractions
  beyond what the task requires.
- No backwards-compatibility hacks: no `// removed` comments, no renaming unused
  vars to `_var`, no shim modules. If something is unused, delete it.
- No fallbacks, defaults, or feature flags unless the PR explicitly asks for them.
- Default to no comments. Well-named identifiers carry the *what*. Reserve
  comments for non-obvious *why* (hidden constraint, workaround, surprising
  invariant).
- Trust internal code and framework guarantees. Only validate at system
  boundaries (user input, external commands, filesystem entry).

## What will get pushed back on

- Suggestions that ignore content under `docs/`.
- Style nits in code that already follows the project's style.
- Defensive error handling for invariants the type system already enforces.
- Comments that restate what the code does.
- Pinning org-internal reusable workflows (e.g. `arthur-debert/dagentic`) to
  SHA — the reusable pattern is "fix once, propagate", and same-owner
  supply-chain risk is negligible.
