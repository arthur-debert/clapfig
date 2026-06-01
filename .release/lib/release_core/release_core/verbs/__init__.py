"""release_core.verbs — the migrated domain verbs.

Each verb module exposes ``def main(argv: list[str]) -> int`` and is driven by a
thin ≤18-line shim on ``$PATH`` (the gh-task-status pattern). Modules are
self-registering and additive — a new verb adds its own file and never edits a
shared registry — so parallel Phase-1 PRs touch disjoint files.
"""
