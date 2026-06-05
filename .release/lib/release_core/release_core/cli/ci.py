"""``release-core ci`` — CI-glue fetch helpers (implemented).

Both commands are backed by standalone ``bin/`` scripts, so they use
:func:`~release_core.cli._helpers.wrap_script`:

  ci fetch-deps        ← bin/fetch-deps
  ci fetch-artifact    ← bin/fetch-artifact

A group module's only contract: define a ``click.Group`` and export it as
``group``. ``cli_entry`` imports ``group`` and attaches it to the root.
"""

from __future__ import annotations

import click

from ._helpers import wrap_script


@click.group(
    name="ci",
    short_help="CI-glue fetch helpers (fetch-deps, fetch-artifact).",
)
def group() -> None:
    """CI-side fetch helpers.

    Thin wrappers over the standalone ``fetch-deps`` / ``fetch-artifact``
    scripts that the release workflows use to move built dependencies and
    named build artifacts between GitHub Actions jobs.
    """


group.add_command(
    wrap_script(
        "fetch-deps",
        name="fetch-deps",
        short_help="Fetch a release's built dependencies for a downstream job.",
    )
)
group.add_command(
    wrap_script(
        "fetch-artifact",
        name="fetch-artifact",
        short_help="Fetch a named build artifact from a release run.",
    )
)
