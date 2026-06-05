"""``release-core admin secrets`` — release-secret provisioning.

Flat ``wrap_verb`` leaves (← old name)::

  install   ← install-release-secrets   release_core.verbs.install_release_secrets.main
  token     ← install-release-token     release_core.verbs.install_release_token.main
"""

from __future__ import annotations

import click

from ...verbs import install_release_secrets, install_release_token
from .._helpers import wrap_verb


@click.group(
    name="secrets",
    short_help="Provision release secrets: install / token.",
)
def group() -> None:
    """Release-secret provisioning for onboarded repos.

    Install the full release secret set, or just the release token, on a repo.
    """


group.add_command(
    wrap_verb(
        install_release_secrets.main,
        name="install",
        short_help="Install the full release secret set on a repo.",
    )
)
group.add_command(
    wrap_verb(
        install_release_token.main,
        name="token",
        short_help="Install the release token on a repo.",
    )
)
