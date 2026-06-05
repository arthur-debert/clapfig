"""``release-core admin repos`` — fleet-repo views.

Flat ``wrap_verb`` leaves over the existing fleet verbs (← old name)::

  list      ← managed-repos           release_core.verbs.managed_repos.main
  prs       ← list-repo-pr            release_core.verbs.list_repo_pr.main
  scripts   ← list-repo-scripts       release_core.verbs.list_repo_scripts.main
  audit     ← audit-portfolio         release_core.verbs.audit_portfolio.main
  verify    ← release-verify-fleet    release_core.verbs.release_verify_fleet.main

The module exports a ``group`` (a ``click.Group``); the ``admin`` assembler
imports and attaches it. Each leaf is a faithful passthrough — argv and
``--help`` go straight to the verb.
"""

from __future__ import annotations

import click

from ...verbs import (
    audit_portfolio,
    list_repo_pr,
    list_repo_scripts,
    managed_repos,
    release_verify_fleet,
)
from .._helpers import wrap_verb


@click.group(
    name="repos",
    short_help="Fleet-repo views: list / prs / scripts / audit / verify.",
)
def group() -> None:
    """Views over the managed fleet repos.

    Read-only-ish lenses on the fleet: the repo roster, open PRs and
    per-repo scripts across it, a whole-portfolio audit, and the hermetic
    pre-flight verify sweep.
    """


group.add_command(
    wrap_verb(
        managed_repos.main,
        name="list",
        short_help="List the managed fleet repos.",
    )
)
group.add_command(
    wrap_verb(
        list_repo_pr.main,
        name="prs",
        short_help="List open PRs across the fleet.",
    )
)
group.add_command(
    wrap_verb(
        list_repo_scripts.main,
        name="scripts",
        short_help="List per-repo scripts across the fleet.",
    )
)
group.add_command(
    wrap_verb(
        audit_portfolio.main,
        name="audit",
        short_help="Audit the whole portfolio.",
    )
)
group.add_command(
    wrap_verb(
        release_verify_fleet.main,
        name="verify",
        short_help="Hermetic pre-flight fleet sweep.",
    )
)
