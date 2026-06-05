"""cli_entry — the top-level ``release-core`` CLI, assembled from the click tree.

``release-core`` is the package's own command (entry
``release_core.cli_entry:main``, wired in pyproject ``[project.scripts]``). It is
the ROOT of a hierarchical click command tree:

    release-core <group> <command> [args...]

This module is a THIN ASSEMBLER: it builds the root ``click.Group`` and attaches
each top-level group, which lives in its OWN module under ``release_core.cli`` —
``toplevel`` (the per-project flat verbs), ``pr``, ``ci``, and the ``admin``
subpackage. Adding or filling a group touches only that group's module, never
this file — that is what lets parallel agents work without colliding. See
``docs/dev/release-core-cli-pattern.md``.

``main(argv) -> int`` is preserved as the entrypoint signature: it is what both
the installed console-script and the local ``bin/release-core`` shim call. The
click→int bridge lives in ``release_core.cli._helpers.run_root``.
"""

from __future__ import annotations

import click

from . import __version__
from .cli import admin, ci, pr, toplevel
from .cli._helpers import run_root


@click.group(
    help=(
        "release-core — the release tooling CLI.\n\n"
        "All infrastructure tasks go through this one tree; --help is the map. "
        "Per-project commands live at the top level (init, cut, status, sync, "
        "pr ...); fleet / meta-release ops live under `admin`."
    ),
    context_settings={"help_option_names": ["-h", "--help"]},
    invoke_without_command=True,
)
@click.version_option(version=__version__, prog_name="release-core")
@click.pass_context
def root(ctx: click.Context) -> None:
    """Root group. Subgroups/commands are attached below by the assembler.

    Bare ``release-core`` (no subcommand) is a discovery entry point: it prints
    the help (the map) and exits 0, rather than click's default usage error.
    """
    if ctx.invoked_subcommand is None:
        click.echo(ctx.get_help())
        ctx.exit(0)


# Per-project flat verbs + small per-project groups (init/selfcheck folded in).
toplevel.attach(root)

# Top-level groups, one module each.
root.add_command(pr.group)
root.add_command(ci.group)
root.add_command(admin.group)


def main(argv: list[str] | None = None) -> int:
    """Entry point: build-and-run the click root, returning an int exit code.

    Signature is preserved (``main(argv) -> int``) so the installed
    console-script (``release_core.cli_entry:main``) and the local
    ``bin/release-core`` shim both keep working unchanged.
    """
    return run_root(root, argv)


if __name__ == "__main__":
    raise SystemExit(main())
