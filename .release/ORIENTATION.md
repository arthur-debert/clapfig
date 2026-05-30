# Welcome — this repo is managed by `release`

This repository's development infrastructure — the quality gate, CI workflows,
build, release, and the PR/dev workflow — is provided by
[`arthur-debert/release`][release] and synced in. It is **not** maintained here.
This note orients you: where you are, what is managed, and where problems go.

## What is managed (don't hand-edit)

- **`.release/`** is the materialized managed tree. `release-sync` regenerates it
  wholesale from `release`; anything inside it is overwritten on the next sync.
- **`bin/`** holds release-provided tools, symlinked into `.release/`. The
  pre-commit gate (`lefthook.yml`), `.github/workflows/`, and the lint configs
  are managed too.
- **`app-bin/`** is this repo's own tooling — yours to edit freely.

Editing a managed file (anything that is a symlink into `.release/`) does not
stick: the next sync replaces it. Changes to managed infrastructure belong
upstream — see **Escalation** below.

## The dev flow at a glance

Pull requests are driven to *ready for human merge* by a reviewer-agnostic state
engine. Rather than piecing together which reviews are pending, which threads
are open, and whether the PR is mergeable, ask the engine where the PR stands:

```sh
gh-task-status <pr-number>
```

It reports the PR's lifecycle state — requested → reviewed → ready — and what is
left before a human can merge. That is the entry point for the flow.

## Escalation — when managed infrastructure breaks

If a gate, workflow, or managed tool misbehaves, do **not** patch it in this
repo: the file is release's and your fix will not survive the next sync. Instead:

1. Unblock locally so your own work proceeds (for a pre-commit gate, a single
   `git commit --no-verify` is the usual escape hatch).
2. Search the [`release` issue tracker][issues] for a matching symptom.
3. If there is none, file one from inside this repo — it auto-collects the repo,
   branch, PR, and failing run for reproduction context.

```sh
gh-release-issue <component> "<one-line symptom>"
```

The fix lands in `release`, is released, and propagates to every consumer when
the floating major you pin (`@v2`) advances — you re-run `release-sync` and pick
it up. One fix, the whole fleet, nothing to hand-edit here.

[release]: https://github.com/arthur-debert/release
[issues]: https://github.com/arthur-debert/release/issues
