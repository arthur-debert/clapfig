"""Advance the floating major branch (v1, v2, …) to the current main
(fast-forward only).

Consumers pin `@vN`; that branch must always point at the latest
non-breaking commit on main. After merging a release-side change to main,
run this to publish it to every `@vN` consumer in one step instead of the
four-command `fetch && checkout vN && merge --ff-only main && push` dance.

The target major is auto-detected as the HIGHEST `origin/vN` branch, so it
tracks the current major (v2 today, v3 later) without re-wiring. Override
with `--major vN`.

Usage:
  release-advance-major              # ff highest vN -> origin/main, then push
  release-advance-major --major v2   # advance a specific major branch
  release-advance-major --dry-run    # show what would happen, push nothing
  release-advance-major <ref>        # ff the major -> <ref> instead of origin/main

Runs from inside the release repo (or $RELEASE_HOME). It updates the
remote major ref directly via a server-side fast-forward push, so it never
touches your working tree or current checkout. Refuses if the
fast-forward is impossible (the major has commits the target doesn't) —
that's a divergence signal (likely a breaking change on main; cut the next
major instead) to resolve by hand.

Exit codes:
  0  — major advanced (or already up to date)
  1  — fatal error / non-fast-forward
  64 — bad usage
"""

from __future__ import annotations

import os
import re
import sys

from .. import gh, proc


def _help_text() -> str:
    """The help body. The bash printed `sed -n '2,/^$/p' | sed 's/^# ?//'` over
    its header — header line 2 to the first TRULY-empty line, which (the `#`
    comment lines never being empty) was the entire comment block. The module
    docstring is that same block verbatim, so we print it whole."""
    return (__doc__ or "").strip("\n")


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash arg loop
    dry_run = False
    ref = ""
    major = ""

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--dry-run":
            dry_run = True
        elif arg == "--major":
            major = argv[i + 1] if i + 1 < len(argv) else ""
            i += 1
        elif arg in ("-h", "--help"):
            print(_help_text())
            return 0
        elif arg.startswith("-"):
            print(f"unknown arg: {arg}", file=sys.stderr)
            print(_help_text(), file=sys.stderr)
            return 64
        else:
            if ref:
                print("release-advance-major: too many args", file=sys.stderr)
                return 64
            ref = arg
        i += 1

    # Locate the release repo: prefer $RELEASE_HOME, else the current repo.
    release_home = os.environ.get("RELEASE_HOME") or os.path.join(
        os.path.expanduser("~"), "release"
    )
    if gh.is_git_worktree(release_home):
        os.chdir(release_home)
    else:
        top = proc.run(["git", "rev-parse", "--show-toplevel"], check=False)
        if top.returncode == 0 and top.stdout.strip():
            os.chdir(top.stdout.strip())
        else:
            print(
                "release-advance-major: not inside the release repo and $RELEASE_HOME unset",
                file=sys.stderr,
            )
            return 1

    # Sanity: refuse to push on a repo that isn't arthur-debert/release.
    origin = proc.run(["git", "remote", "get-url", "origin"], check=False)
    origin_url = origin.stdout.strip() if origin.returncode == 0 else ""
    if "arthur-debert/release" not in origin_url:
        print(
            f"release-advance-major: origin ('{origin_url}') doesn't look like "
            "arthur-debert/release; refusing",
            file=sys.stderr,
        )
        return 1

    proc.run(["git", "fetch", "--quiet", "origin"])

    # Resolve the target major branch: explicit --major, else the highest origin/vN.
    if not major:
        major = _highest_major()
        if not major:
            print(
                "release-advance-major: no origin/vN branch found; pass --major vN",
                file=sys.stderr,
            )
            return 1
    if not re.match(r"^v[0-9]", major):
        print(
            f"release-advance-major: --major must look like vN (got '{major}')",
            file=sys.stderr,
        )
        return 64

    src = ref or "origin/main"
    target = proc.run(["git", "rev-parse", "--verify", src], check=False)
    if target.returncode != 0:
        print(f"release-advance-major: bad ref '{src}'", file=sys.stderr)
        return 1
    target_sha = target.stdout.strip()
    target_short = proc.out(["git", "rev-parse", "--short", target_sha])

    exists = proc.run(["git", "rev-parse", "--verify", "--quiet", f"origin/{major}"], check=False)
    if exists.returncode == 0 and exists.stdout.strip():
        current = proc.out(["git", "rev-parse", f"origin/{major}"])
        current_short = proc.out(["git", "rev-parse", "--short", current])
        if current == target_sha:
            print(f"{major} already at {src} ({target_short}) — nothing to do.")
            return 0
        ancestor = proc.run(
            ["git", "merge-base", "--is-ancestor", current, target_sha], check=False
        )
        if ancestor.returncode != 0:
            print(
                f"release-advance-major: {major} ({current_short}) is NOT an ancestor "
                f"of {src} ({target_short}).",
                file=sys.stderr,
            )
            print(
                f"A fast-forward is impossible — {major} has commits {src} doesn't "
                "(likely a breaking",
                file=sys.stderr,
            )
            print(
                "change landed on main; cut the next major instead). Resolve by hand.",
                file=sys.stderr,
            )
            return 1
        print(f"advancing {major}: {current_short} -> {target_short} ({src})")
    else:
        print(
            f"release-advance-major: origin/{major} doesn't exist yet; creating it at "
            f"{src} ({target_short}).",
            file=sys.stderr,
        )

    if dry_run:
        print(f"(dry-run) would: git push origin {target_sha}:refs/heads/{major}")
        return 0

    proc.run(["git", "push", "origin", f"{target_sha}:refs/heads/{major}"])
    print(f"{major} -> {target_short}")
    return 0


def _highest_major() -> str:
    """The highest `origin/vN` branch name (e.g. 'v2'), or '' if none.

    Mirrors `git branch -r | grep -oE 'origin/v[0-9]+$' | sed 's|origin/||'
    | sort -V | tail -1`."""
    res = proc.run(["git", "branch", "-r"], check=False)
    if res.returncode != 0:
        return ""
    majors: list[tuple[int, str]] = []
    for line in res.stdout.splitlines():
        m = re.search(r"origin/v([0-9]+)$", line.strip())
        if m:
            majors.append((int(m.group(1)), f"v{m.group(1)}"))
    if not majors:
        return ""
    majors.sort()
    return majors[-1][1]
