"""``release-core pr`` — the PR-loop helpers (EXEMPLAR group).

This group is the reference implementation for the parallel agents. It shows,
in one place, every pattern they need:

- a nested subgroup (``pr copilot`` with ``on``/``off``/``wait``/``review``),
- :func:`~release_core.cli._helpers.wrap_script` for the standalone bash/python
  tools (``gh-copilot-*``, ``gh-pr-checks-wait``, ``gh-pr-resolve-thread``),
- :func:`~release_core.cli._helpers.wrap_verb` for a native verb
  (``pr status`` → the folded PR-state engine, ``prstate.cli.task_status``).

A group module's only contract: define a ``click.Group`` and export it as
``group``. ``cli_entry`` imports ``group`` and attaches it to the root.
"""

from __future__ import annotations

import click

from ..prstate.cli import task_status
from ._helpers import wrap_script, wrap_verb


@click.group(
    name="copilot",
    short_help="Drive the Copilot review on a PR (on/off/wait/review).",
)
def copilot() -> None:
    """Request, dismiss, wait on, or re-trigger a Copilot review on a PR.

    Thin nested group over the ``gh-copilot-*`` tools — the canonical
    arthur-debert review-loop primitives.
    """


copilot.add_command(
    wrap_script(
        "gh-copilot-on",
        name="on",
        short_help="Request a Copilot review on a PR.",
    )
)
copilot.add_command(
    wrap_script(
        "gh-copilot-off",
        name="off",
        short_help="Dismiss / stop requesting Copilot review on a PR.",
    )
)
copilot.add_command(
    wrap_script(
        "gh-copilot-wait",
        name="wait",
        short_help="Block until Copilot's review lands on a PR.",
    )
)
copilot.add_command(
    wrap_script(
        "gh-copilot-review",
        name="review",
        short_help="Re-trigger a fresh Copilot review pass on a PR.",
    )
)


@click.group(
    name="pr",
    short_help="PR-loop helpers: Copilot review, checks-wait, threads, status.",
)
def group() -> None:
    """Per-project PR-loop helpers.

    Everything an agent or human needs to drive a single PR through the
    arthur-debert review pipeline: request and wait on the Copilot review,
    wait on CI checks, resolve review threads, and read the PR's lifecycle
    state (``status``).
    """


group.add_command(copilot)
group.add_command(
    wrap_script(
        "gh-pr-checks-wait",
        name="checks-wait",
        short_help="Block until a PR's required checks finish.",
    )
)
group.add_command(
    wrap_script(
        "gh-pr-resolve-thread",
        name="resolve-thread",
        short_help="Resolve a PR review thread (or all addressed threads).",
    )
)
group.add_command(
    wrap_verb(
        task_status.main,
        name="status",
        short_help="Where does this PR stand? (lifecycle state + next action)",
    )
)
