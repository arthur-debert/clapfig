This directory is the source of truth for the project's changelog.

Layout:

  CHANGELOG/
    README.txt              # this file
    legacy.md               # (optional) pre-adoption history, appended verbatim by render
    unreleased-<slug>.md    # one per in-flight change (slug = PR# or short kebab tag)
    <version>.md            # one per released version, written by `changelog-cut`

In a FEATURE PR, add a FRAGMENT and nothing else:

    changelog add <PR#> "- Short description of the change (#PR#)"

Do NOT regenerate CHANGELOG.md and do NOT commit it in a feature PR. The
repo-root CHANGELOG.md is rendered from these fragments at RELEASE-CUT time
(`changelog cut` + `render`, via the release hook), not per feature PR. Never
hand-edit CHANGELOG.md either — your fragment here is the only thing a feature
PR touches; the render is the release step's job.

Tools (the `changelog` console-script, installed via the release_core
pip wheel — `install-release-core` at SessionStart):

  changelog add [--force] <slug> [body...]
      Add an unreleased fragment. Body comes from stdin when no body args
      are given, otherwise from the joined args. Numeric slug → "pr-N".
      Fails if a fragment with the same slug exists; --force overwrites.

      Examples:
        changelog add 142 "- Fix tokenizer crash on empty input (#142)"
        changelog add fix-token-leak <<'EOF'
        - Fix token leak in retry path
        - Bonus: emit retry count in --json output
        EOF

  changelog cut <version>
      Concat all CHANGELOG/unreleased-*.md into CHANGELOG/<version>.md
      with a "## <version> - YYYY-MM-DD" header, then delete the
      fragments. Typically invoked via `new-version` from a release hook.

  changelog render
      Regenerate CHANGELOG.md from the directory contents. Idempotent.
      This is a RELEASE-TIME step (run by the release hook), not something to
      run or commit inside a feature PR.

  changelog new-version <version>
      Convenience: cut + render.

Conventions:

- Fragment slugs: prefer PR numbers (auto-prefixed "pr-"). Fall back to a
  short kebab-case tag (`fix-token-leak`, `add-json-output`) for non-PR
  flows. The slug never appears in the rendered output — it exists only
  to avoid filename collisions between concurrent fragments.

- Fragment body: plain markdown bullets, no version header, no date.
  Multi-line OK; pipe through stdin to preserve line structure.

- Version filenames: strict semver (`1.2.3`, `1.2.3-rc.1`). Render fails
  loud on anything that doesn't parse — bypassing this rule is a bug.

- `legacy.md`: optional. Created during migration from a pre-fragment
  changelog convention by stripping the top-level `# Changelog` heading.
  Render appends it verbatim at the end of CHANGELOG.md, so the
  pre-adoption history stays visible without retroactive per-version
  splitting.
