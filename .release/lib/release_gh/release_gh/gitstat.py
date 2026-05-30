"""Git-backed diff sizes for the diff-trajectory breaker.

Returns a callable(commit_id) -> total changed lines vs the merge-base with the
base branch, or None when git can't answer (commit not fetched locally, no base
ref) — in which case the diff-trajectory breaker degrades to skipped rather than
guessing. Lives outside the pure core because it shells out to git.
"""

from __future__ import annotations

import subprocess

from .breakers import DiffSizer


def diff_sizer(base_ref: str | None) -> DiffSizer | None:
    """Build a diff_sizer for `base_ref`, or None if no base is known."""
    if not base_ref:
        return None

    def size(commit_id: str) -> int | None:
        try:
            merge_base = subprocess.run(
                ["git", "merge-base", base_ref, commit_id],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            numstat = subprocess.run(
                ["git", "diff", "--numstat", f"{merge_base}..{commit_id}"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
        except (subprocess.CalledProcessError, FileNotFoundError):
            return None
        total = 0
        for line in numstat.splitlines():
            added, _, rest = line.partition("\t")
            deleted, _, _ = rest.partition("\t")
            total += int(added) if added.isdigit() else 0
            total += int(deleted) if deleted.isdigit() else 0
        return total

    return size
