"""``release-core admin policy`` — GitHub policy ops.

Flat ``wrap_verb`` leaves (← old name)::

  ruleset      ← apply-ruleset               release_core.verbs.apply_ruleset.main
  sweep        ← sweep-github-policy          release_core.verbs.sweep_github_policy.main
  dependabot   ← enable-dependabot-security   release_core.verbs.enable_dependabot_security.main
"""

from __future__ import annotations

import click

from ...verbs import apply_ruleset, enable_dependabot_security, sweep_github_policy
from .._helpers import wrap_verb


@click.group(
    name="policy",
    short_help="GitHub policy ops: ruleset / sweep / dependabot.",
)
def group() -> None:
    """GitHub policy administration for fleet repos.

    Apply the canonical branch ruleset, sweep / reconcile broader GitHub
    policy, and enable Dependabot security updates on onboarded repos.
    """


group.add_command(
    wrap_verb(
        apply_ruleset.main,
        name="ruleset",
        short_help="Apply the canonical branch ruleset to a repo.",
    )
)
group.add_command(
    wrap_verb(
        sweep_github_policy.main,
        name="sweep",
        short_help="Sweep / reconcile a repo's GitHub policy settings.",
    )
)
group.add_command(
    wrap_verb(
        enable_dependabot_security.main,
        name="dependabot",
        short_help="Enable Dependabot security updates on a repo.",
    )
)
