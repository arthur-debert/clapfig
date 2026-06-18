# Release-managed orientation

<!-- Managed by release-core; do not edit. Regenerate via release-core init. -->

This repo's quality gate, build, release, and PR/dev flow are provided by
`release-core` (installed at session start; not stored in this repo).

- **Start here:** run `release-core how-to` — the task playbook for *this* repo
  (its dev cycle, incl. coordinating a complex / multi-PR feature with subagents).
- Reference: `release-core --help`, `release-core <cmd> --help`, `release-core detect-kind`.
- Quality gate (run every loop, after `git add`): `release-core gate`.
