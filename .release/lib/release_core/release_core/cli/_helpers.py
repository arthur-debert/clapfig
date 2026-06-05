"""Shared click helpers for the ``release-core`` command tree.

Two — and only two — wrapping patterns back every leaf command, so every
group module (and every parallel agent filling one in) does it identically:

- :func:`wrap_verb` — a click command that forwards its argv to a
  ``release_core.verbs.<verb>.main(argv) -> int`` (or any ``main(argv) -> int``
  callable, e.g. ``prstate.cli.task_status.main``). Behavior is byte-identical
  to invoking the verb directly: the verb owns its own arg parsing AND its own
  ``--help`` (we forward ``--help`` straight through to the verb's docstring
  help). The click layer here is a pure passthrough — it never re-parses.

- :func:`wrap_script` — a click command that subprocess-execs an existing
  ``bin/<script>`` (the standalone bash/python tools: ``gh-copilot-*``,
  ``gh-pr-checks-wait``, ``gh-pr-resolve-thread``, ``fetch-deps``,
  ``fetch-artifact``). It forwards args verbatim and propagates the child's
  exit code. The script is resolved off ``$PATH`` by name — these tools are on
  ``$PATH`` in every environment that has them (dodot locally, action_path in
  CI), so we never compute a path relative to the installed wheel.

Both helpers deliberately disable click's own option parsing on the leaf
(``ignore_unknown_options`` + ``allow_extra_args`` via ``PASSTHROUGH_CONTEXT``,
a catch-all ``args`` argument, and ``add_help_option=False``). That is what
makes the passthrough faithful: ``release-core pr copilot wait --json 91`` must
reach the underlying tool with ``--json 91`` untouched, not be re-interpreted
by click.

Help convention (a first-class requirement of #460):
- Every leaf gets a one-line ``short_help`` — that is the text shown in the
  PARENT group's ``--help`` listing. Keep it terse and imperative.
- The full ``--help`` for a leaf is delegated to the underlying tool: for a
  verb-wrap, the verb's own docstring/USAGE; for a script-wrap, the script's
  own ``--help``. So ``release-core <group> <cmd> --help`` shows the real,
  authoritative help, never a thin click re-statement of it.

Stubs: :func:`stub_group` builds a registered-but-unimplemented GROUP whose bare
invocation prints help + a stub note and exits :data:`STUB_EXIT` (never a silent
0). The matching stub LEAF factory is ``toplevel._stub_command``.
"""

from __future__ import annotations

import errno
import subprocess
import sys
from collections.abc import Callable

import click

# A verb entrypoint: ``main(argv: list[str]) -> int``.
VerbMain = Callable[[list[str]], int]

# click context settings that turn a command into a faithful passthrough:
# accept any flags/args without click trying to parse them.
PASSTHROUGH_CONTEXT = {
    "ignore_unknown_options": True,
    "allow_extra_args": True,
    "help_option_names": [],  # do NOT let click intercept -h/--help
}

# Exit code for a registered-but-unimplemented command/group (EX_UNAVAILABLE).
# A stub never silently succeeds: invoking it prints a clear message and exits
# with this code, so it can't be mistaken for a working command.
STUB_EXIT = 69


def stub_group(name: str, *, short_help: str, help: str | None = None) -> click.Group:
    """Build a registered-but-unimplemented GROUP (for a parallel agent to fill).

    Shows in the parent ``--help`` like any group; ``<group> --help`` lists its
    (possibly all-stub) subcommands. But invoking the group BARE
    (``release-core <group>`` with no subcommand) prints the help plus a stub
    note and exits :data:`STUB_EXIT`, rather than click's default of exiting 0
    silently — so an empty stub group can't be mistaken for a working command.

    A parallel agent fills the group by ``add_command``-ing real leaves; once the
    bare form is meant to do something (e.g. ``admin inbox`` → release-inbox),
    swap this for a normal ``@click.group(invoke_without_command=True)`` with the
    real callback.
    """

    @click.group(
        name=name,
        short_help=short_help,
        help=help,
        invoke_without_command=True,
    )
    @click.pass_context
    def _grp(ctx: click.Context) -> None:
        if ctx.invoked_subcommand is None:
            click.echo(ctx.get_help())
            click.echo(
                f"\nrelease-core: `{name}` is a registered stub group (no bare "
                f"behavior yet — see #460).",
                err=True,
            )
            raise SystemExit(STUB_EXIT)

    return _grp


def wrap_verb(
    verb_main: VerbMain,
    *,
    name: str,
    short_help: str,
) -> click.Command:
    """Build a passthrough click command that forwards argv to ``verb_main``.

    The resulting command does no parsing of its own: every token after the
    command name (including ``--help``) is forwarded verbatim to
    ``verb_main(argv)``, and that callable's ``int`` return becomes the process
    exit code. This guarantees byte-identical behavior — same stdout/stderr,
    same exit codes, same ``--help`` — as invoking the verb directly.

    ``name`` is the command name as it appears in the tree (e.g. ``"cut"``).
    ``short_help`` is the one-liner shown in the parent group's ``--help``.
    """

    @click.command(
        name=name,
        short_help=short_help,
        context_settings=PASSTHROUGH_CONTEXT,
        add_help_option=False,
    )
    @click.argument("args", nargs=-1, type=click.UNPROCESSED)
    def _cmd(args: tuple[str, ...]) -> None:
        raise SystemExit(verb_main(list(args)))

    return _cmd


def wrap_script(
    script: str,
    *,
    name: str,
    short_help: str,
) -> click.Command:
    """Build a passthrough click command that execs the ``bin/<script>`` tool.

    ``script`` is the on-``$PATH`` command name (e.g. ``"gh-copilot-wait"``).
    Args are forwarded verbatim and the child's exit code is propagated. The
    full ``--help`` is the script's own (``--help`` forwards straight through),
    so the authoritative help is shown, never a click re-statement.

    ``name`` is the command name in the tree; ``short_help`` is the one-liner
    shown in the parent group's ``--help``.
    """

    @click.command(
        name=name,
        short_help=short_help,
        context_settings=PASSTHROUGH_CONTEXT,
        add_help_option=False,
    )
    @click.argument("args", nargs=-1, type=click.UNPROCESSED)
    def _cmd(args: tuple[str, ...]) -> None:
        try:
            completed = subprocess.run([script, *args], check=False)
        except OSError as exc:
            if exc.errno == errno.ENOENT:
                click.echo(
                    f"release-core: required tool {script!r} not found on $PATH",
                    err=True,
                )
                raise SystemExit(127) from None
            # Exists but cannot be executed (e.g. EACCES): 126, the shell's
            # "command invoked cannot execute" convention.
            click.echo(
                f"release-core: cannot execute {script!r}: {exc}",
                err=True,
            )
            raise SystemExit(126) from None
        # A child killed by signal N reports returncode -N; normalize to the
        # shell's 128+N convention so the parent never sees a negative code.
        rc = completed.returncode
        raise SystemExit(128 + abs(rc) if rc < 0 else rc)

    return _cmd


def run_root(root: click.Group, argv: list[str] | None = None) -> int:
    """Invoke a click ``root`` group as a ``main(argv) -> int`` entrypoint.

    Bridges click (which raises ``SystemExit``) to the ``main(argv) -> int``
    convention that the console-script wrapper and the local ``bin/`` shim
    expect. ``standalone_mode=False`` lets click return a value / raise its own
    exceptions instead of calling ``sys.exit`` itself; we normalize all the
    exit paths here so the caller always gets an ``int``.
    """
    args = list(sys.argv[1:] if argv is None else argv)
    try:
        root.main(args=args, prog_name="release-core", standalone_mode=False)
    except SystemExit as exc:  # leaf passthroughs raise SystemExit(code)
        code = exc.code
        return code if isinstance(code, int) else (0 if code is None else 1)
    except click.exceptions.Exit as exc:
        # Clean-exit signal (e.g. --help / --version / ctx.exit). Some click 8.x
        # versions return the code instead of raising; catch both so either way
        # we surface the integer exit code rather than leaking the exception.
        return exc.exit_code
    except click.exceptions.Abort:
        click.echo("Aborted!", err=True)
        return 1
    except click.exceptions.UsageError as exc:
        exc.show()
        return exc.exit_code  # 2, click's usage-error convention
    except click.exceptions.ClickException as exc:
        exc.show()
        return exc.exit_code
    return 0
