# Welcome — this repo is managed by `release`

This repository's development infrastructure — the quality gate, CI workflows,
build, release, and the PR/dev workflow — is provided by
[`arthur-debert/release`][release] and synced in. It is **not** maintained here.
This note orients you: where you are, what is managed, and where problems go.

## What is managed (don't hand-edit)

- **`.release/`** is the materialized managed tree. `release-core sync` regenerates
  it wholesale from `release`; anything inside it is overwritten on the next sync.
- **`bin/`** holds release-provided tools, symlinked into `.release/`. The
  pre-commit gate (`lefthook.yml`), `.github/workflows/`, and the lint configs
  are managed too. Every managed task is run through the **`release-core`** CLI —
  `release-core --help` is the map (per-repo commands at the top level; fleet ops
  under `release-core admin`).
- **`app-bin/`** is this repo's own tooling — yours to edit freely.

Editing a managed file (anything that is a symlink into `.release/`) does not
stick: the next sync replaces it. Changes to managed infrastructure belong
upstream — see **Escalation** below.

## Skills (managed too — don't hand-copy)

This repo carries `release`'s official infrastructure and dev-cycle skills under
`.claude/skills/` — the PR review loop, review-response, upstream escalation, and
general dev skills (TDD, review, triage, diagnose, and more). They are **managed
and synced**, just like the rest of `.release/`: each is a symlink into the
materialized tree, regenerated on every sync.

- **Use the synced skills as-is.** Do **not** hand-edit or hand-copy them into
  this repo. A hand-copied skill drifts out of step with upstream — the synced
  symlink is what keeps your copy current. If a skill needs a fix, that is an
  upstream change (see **Escalation**).
- **Only application-domain skills are this repo's own.** Skills specific to this
  project's subject matter live here and are yours to maintain; the infra/dev
  skills are not.

## The dev flow at a glance

Pull requests are driven to _ready for human merge_ by a reviewer-agnostic state
engine. Rather than piecing together which reviews are pending, which threads
are open, and whether the PR is mergeable, you ask the engine where the PR
stands and act on what it reports:

```sh
release-core pr status <pr-number>
```

It reports the PR's lifecycle state — **requested → reviewed → ready** — and what
is left before a human can merge. That is the entry point. Opening the PR is not
the end of your job; you own it through the whole loop:

1. **Open a _live_ PR** (never a draft — a draft suppresses the automatic
   review). The review is requested for you.
2. **Poll `release-core pr status <pr>`.** It names what is outstanding: a pending
   review, unresolved threads, or failing checks.
3. **Clear what it names.** Fix the code or reply with a rationale, resolve each
   thread, push, and let checks go green. Never bypass the gate (`--no-verify`)
   to force a check past — fix the cause; CI re-runs the same gate on a clean
   runner.
4. **Repeat until the state is `ready`.** Reviews can lag — wait for them rather
   than declaring done early.
5. **Stop at `ready`; a human merges.** Don't self-merge. The final read and the
   merge are the human's.

That is the whole loop: open → poll the engine → clear what it names → `ready` →
hand off. Drive it through `release-core pr status`; don't reinvent it with
ad-hoc `gh api` calls.

**Landing a feature or fix?** Add a changelog fragment in the same PR:

```sh
release-core changelog add <slug> "<one-line summary>"
```

It writes `CHANGELOG/unreleased-<slug>.md`. The release refuses to cut without
one — the prepare gate fails with _"No CHANGELOG/unreleased-\*.md fragments
found"_ — so a feature that merges without a fragment silently blocks the next
release until someone backfills it.

## Escalation — when managed infrastructure breaks

If a gate, workflow, or managed tool misbehaves, do **not** patch it in this
repo: the file is release's and your fix will not survive the next sync. Instead:

1. Unblock locally so your own work proceeds (for a pre-commit gate, a single
   `git commit --no-verify` is the usual escape hatch).
2. Search the [`release` issue tracker][issues] for a matching symptom.
3. If there is none, file one from inside this repo — it auto-collects the repo,
   branch, PR, and failing run for reproduction context.

```sh
release-core issue file <component> "<one-line symptom>"
```

The fix lands in `release`, is released, and propagates to every consumer when
the floating major you pin (`@v2`) advances — you re-run `release-core sync` and
pick it up. One fix, the whole fleet, nothing to hand-edit here.

[release]: https://github.com/arthur-debert/release
[issues]: https://github.com/arthur-debert/release/issues
