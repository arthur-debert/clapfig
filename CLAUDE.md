# CLAUDE.md

## Tooling

This repo follows the canonical tooling described in [`~/h/repo-all/tooling.lex`](../repo-all/tooling.lex). Short summary:

- **Branch protection**: `main` is gated by the `main-branch-protection` ruleset — PRs are required and status checks must be green to merge.
- **Copilot review**: `.github/workflows/copilot-review.yml` auto-triggers Copilot on every PR via the reusable workflow at [`arthur-debert/gh-dagentic`](https://github.com/arthur-debert/gh-dagentic). Do NOT pin the reusable to a SHA — same-owner reusables follow "fix once, propagate".
- **Policy files** (`CODEOWNERS`, `dependabot.yml`, `copilot-instructions.md`, `pull_request_template.md`) are synced from `~/h/dotfiles/gh/templates/rust/`. Edit the source-of-truth template first when changes should propagate.
- **Releases run in CI**: `scripts/release` triggers `.github/workflows/release.yml`. Local `cargo publish` is not the path. Crate publishes use `scripts/ci-publish-crate.sh` for tolerant retry behavior (handles re-runs of older rcs and post-partial-publish recovery).
