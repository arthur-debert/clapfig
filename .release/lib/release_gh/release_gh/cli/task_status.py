"""`gh-task-status` — read-only PR lifecycle snapshot.

Prints where a PR stands (one of the TaskState values) and the single next
action. Resolves the PR number for the current branch when no number is given.
Read-only: it never edits the PR — it reports READY; the caller flips.
"""

from __future__ import annotations

import json
import sys

from .. import ghapi, gitstat
from ..fetch import gather
from ..state import TaskState, TaskStatus, evaluate, no_pr

USAGE = """\
gh-task-status — where does this PR stand?

Usage:
  gh-task-status [<pr-number>] [--json]

With no <pr-number>, resolves the PR number for the current branch. Read-only:
reports the lifecycle state (reviews pending/addressing/reviewed/validating/
ready/blocked) and the next action; never mutates the PR.

Options:
  --json     emit the status as a JSON object
  -h --help  show this help

Exit codes:
  0   status printed
  64  bad usage
"""


def main(argv: list[str]) -> int:
    pr_arg: str | None = None
    as_json = False
    for arg in argv:
        if arg in ("-h", "--help"):
            print(USAGE)
            return 0
        if arg == "--json":
            as_json = True
        elif arg.startswith("-"):
            print(f"error: unknown option {arg}", file=sys.stderr)
            return 64
        elif pr_arg is None:
            pr_arg = arg
        else:
            print("error: too many arguments", file=sys.stderr)
            return 64

    if pr_arg is not None and not pr_arg.isdigit():
        print(f"error: PR number must be numeric (got: {pr_arg})", file=sys.stderr)
        return 64

    pr = int(pr_arg) if pr_arg is not None else _current_pr()
    if pr is None:
        _emit(no_pr(), as_json=as_json)
        return 0
    ctx = gather(pr)
    status = evaluate(ctx, diff_sizer=gitstat.diff_sizer(ctx.base_ref))
    _emit(status, as_json=as_json)
    return 0


def _current_pr() -> int | None:
    """Resolve the PR number for the current branch, or None if there is none."""
    try:
        data = json.loads(ghapi._gh(["pr", "view", "--json", "number"]))
    except (ghapi.GhError, json.JSONDecodeError):
        return None
    return data.get("number")


def _emit(status: TaskStatus, *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(status.to_dict(), indent=2))
        return
    if status.state is TaskState.NO_PR:
        print("state:  NO_PR")
        print(f"next:   {status.next_action}")
        return
    reviewers = "  ".join(f"{name}={lc}" for name, lc in status.reviewers.items())
    print(f"PR #{status.pr}")
    print(f"state:      {status.state.value.upper()}")
    print(f"next:       {status.next_action}")
    print(f"reviewers:  {reviewers}")
    print(f"threads:    {status.open_threads} open")
    print(f"checks:     {status.checks.value}")
    print(f"mergeable:  {status.mergeable}")
    print(f"cycles:     {status.cycles}")
    if status.breaker:
        print(f"breaker:    {status.breaker}")
