"""``release-core admin release`` — release-side mechanics.

Module is named ``release_cmds`` (not ``release``) to avoid shadowing the
package name. Flat ``wrap_verb`` leaves over the existing verbs (← old name)::

  advance-major  ← release-advance-major   release_core.verbs.release_advance_major.main
  betas          ← release-beta-list       release_core.verbs.release_beta_list.main
  lex            ← release-lex             release_core.verbs.release_lex.main

The module exports a ``group`` (a ``click.Group``); the ``admin`` assembler
imports and attaches it. Each leaf is a faithful passthrough — argv and
``--help`` go straight to the verb.
"""

from __future__ import annotations

import click

from ...verbs import release_advance_major, release_beta_list, release_lex
from .._helpers import wrap_verb


@click.group(
    name="release",
    short_help="Release-side mechanics: advance-major / betas / lex.",
)
def group() -> None:
    """Release-side mechanics for the fleet.

    The moves that happen on the release side after a merge: fast-forward
    the floating major branch, list beta/RC releases, and drive the
    release-lex pipeline.
    """


group.add_command(
    wrap_verb(
        release_advance_major.main,
        name="advance-major",
        short_help="Fast-forward the floating major branch.",
    )
)
group.add_command(
    wrap_verb(
        release_beta_list.main,
        name="betas",
        short_help="List beta/RC releases.",
    )
)
group.add_command(
    wrap_verb(
        release_lex.main,
        name="lex",
        short_help="Drive the release-lex pipeline.",
    )
)
