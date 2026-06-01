"""List open release/beta/* branches in $RELEASE_HOME with age + commits
ahead of main. Convention is delete-on-merge; this is the report that
makes stale betas visible.

Usage:
  release-beta-list

Reads $RELEASE_HOME (default $HOME/release). Fetches origin first so
the output reflects the remote, not stale local refs.

Output columns:
  branch         age   ahead-of-main

Exit codes:
  0  — listed (no open betas is still success; prints "(none)")
  1  — fatal error
"""

from __future__ import annotations

import os
import sys

from .. import proc


def _help_text() -> str:
    """The help body. The bash printed `sed -n '2,/^$/p' | sed 's/^# ?//'` over
    its header — header line 2 to the first TRULY-empty line, which (the `#`
    comment lines never being empty) was the entire comment block. The module
    docstring is that same block verbatim, so we print it whole."""
    return (__doc__ or "").strip("\n")


def main(argv: list[str]) -> int:
    arg = argv[0] if argv else ""
    if arg in ("-h", "--help"):
        print(_help_text())
        return 0
    if arg != "":
        print(f"unknown arg: {arg}", file=sys.stderr)
        print(_help_text(), file=sys.stderr)
        return 64

    release_home = os.environ.get("RELEASE_HOME") or os.path.join(
        os.path.expanduser("~"), "release"
    )
    if not os.path.isdir(os.path.join(release_home, ".git")):
        print(
            f"release-beta-list: $RELEASE_HOME='{release_home}' is not a git clone",
            file=sys.stderr,
        )
        return 1

    proc.run(["git", "-C", release_home, "fetch", "--quiet", "--prune", "origin"])

    # Collect betas with their relative age, tab-separated.
    res = proc.run(
        [
            "git",
            "-C",
            release_home,
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(committerdate:relative)",
            "refs/remotes/origin/release/beta/",
        ],
        check=False,
    )
    betas = res.stdout.strip() if res.returncode == 0 else ""

    if not betas:
        print("(none)")
        return 0

    print(f"{'branch':<44}  {'age':<18}  ahead-of-main")
    print(f"{'------':<44}  {'---':<18}  -------------")

    for line in betas.splitlines():
        ref, _, age = line.partition("\t")
        short = ref[len("origin/") :] if ref.startswith("origin/") else ref
        count = proc.run(
            ["git", "-C", release_home, "rev-list", "--count", f"origin/main..{ref}"],
            check=False,
        )
        ahead = count.stdout.strip() if count.returncode == 0 and count.stdout.strip() else "?"
        print(f"{short:<44}  {age:<18}  {ahead}")
    return 0
