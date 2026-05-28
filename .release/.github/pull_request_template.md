# Pull Request

## Summary

<!-- 1-3 sentences: what changed and why. -->

## Checklist

- [ ] Changelog updated (`bin/changelog add <slug>` to add a `CHANGELOG/unreleased-*.md` fragment; `CHANGELOG.md` is generated) — or chore/docs-only
- [ ] Project umbrella check passes locally — `bin/check` (canonical: composes `bin/check-fmt` + `bin/check-lint` + `bin/check-tests`). Bare equivalent: `cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features --no-tests=pass` (the `bin/check-tests` script falls back to `cargo test --all-features` when cargo-nextest isn't installed).
- [ ] Tests added or updated for behavior changes

## Notes for reviewers

<!-- Optional: context to help triage Copilot's review faster. -->
