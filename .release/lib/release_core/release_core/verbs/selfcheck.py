"""selfcheck — prove release-core's third-party dependencies resolved at install.

Usage:
  release-core selfcheck

release_core now ships real third-party dependencies (click), resolved by the
pull-model boot now that `install-release-core` no longer passes `--no-deps`.
This verb is the runtime canary for that contract: it is implemented WITH click
and reports the
importable click version. If a boot ever regresses to a `--no-deps` install,
`import click` at the top of this module raises ModuleNotFoundError and the whole
`release-core` CLI fails loudly — which is exactly the signal we want.

Exit codes:
  0  — dependencies importable (prints the resolved click version)
"""

from __future__ import annotations

import importlib.metadata

import click

from ..cli import EXIT_OK, parse


def main(argv: list[str] | None = None) -> int:
    try:
        parse(argv if argv is not None else [], [], doc=__doc__ or "")
    except SystemExit as exc:  # parse raises SystemExit(0) on -h/--help
        return int(exc.code or 0)

    version = importlib.metadata.version("click")
    click.echo(
        click.style("release-core: ", bold=True) + f"dependencies OK — click {version} importable."
    )
    return EXIT_OK
