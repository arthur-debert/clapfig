"""release_core.cli._helpers — the two wrapping patterns + the click→int bridge.

These pin the contract the parallel per-group agents rely on:

- ``wrap_verb`` forwards argv verbatim (incl. ``--help``) to a
  ``main(argv) -> int`` and propagates the exit code, doing NO parsing of its
  own (passthrough).
- ``wrap_script`` execs a ``bin/<script>`` on ``$PATH``, forwarding args and
  propagating the child exit code; a missing script is a clear 127.
- ``run_root`` bridges a click group to ``main(argv) -> int``.
"""

from __future__ import annotations

import click
from release_core.cli._helpers import (
    STUB_EXIT,
    run_root,
    stub_group,
    wrap_script,
    wrap_verb,
)


def _invoke(cmd: click.Command, args: list[str]) -> int:
    """Run a leaf command standalone, capturing its SystemExit code."""

    @click.group()
    def root() -> None:
        pass

    root.add_command(cmd)
    return run_root(root, [cmd.name, *args])


def test_wrap_verb_forwards_argv_and_exit_code():
    seen: dict = {}

    def verb_main(argv: list[str]) -> int:
        seen["argv"] = argv
        return 3

    cmd = wrap_verb(verb_main, name="thing", short_help="do a thing")
    rc = _invoke(cmd, ["--flag", "pos", "--k=v"])
    assert rc == 3
    # passthrough: every token reaches the verb untouched, in order.
    assert seen["argv"] == ["--flag", "pos", "--k=v"]


def test_wrap_verb_does_not_intercept_help():
    seen: dict = {}

    def verb_main(argv: list[str]) -> int:
        seen["argv"] = argv
        return 0

    cmd = wrap_verb(verb_main, name="thing", short_help="do a thing")
    _invoke(cmd, ["--help"])
    # --help is forwarded to the verb, NOT consumed by click.
    assert seen["argv"] == ["--help"]


def test_wrap_verb_short_help_is_set():
    cmd = wrap_verb(lambda a: 0, name="thing", short_help="one-liner here")
    assert cmd.short_help == "one-liner here"


def test_wrap_script_execs_and_propagates_exit_code():
    # `true` / `false` are on PATH everywhere; use them to prove exec + code.
    ok = wrap_script("true", name="ok", short_help="always ok")
    bad = wrap_script("false", name="bad", short_help="always fails")
    assert _invoke(ok, []) == 0
    assert _invoke(bad, []) == 1


def test_wrap_script_forwards_args():
    # `expr 2 + 3` exits 0 and prints 5; we only assert the exit code path here
    # (arg forwarding is what makes expr succeed rather than usage-error).
    cmd = wrap_script("expr", name="e", short_help="evaluate")
    assert _invoke(cmd, ["2", "+", "3"]) == 0


def test_wrap_script_missing_tool_is_127():
    cmd = wrap_script("release-core-nonexistent-xyz", name="missing", short_help="n/a")
    assert _invoke(cmd, []) == 127


def test_wrap_script_non_executable_is_126(tmp_path):
    # A file that exists but lacks the +x bit raises OSError(EACCES), not
    # FileNotFoundError → 126 ("command invoked cannot execute"), not 127.
    bad = tmp_path / "not-executable.sh"
    bad.write_text("#!/bin/sh\n")
    bad.chmod(0o644)
    cmd = wrap_script(str(bad), name="ne", short_help="non-exec")
    assert _invoke(cmd, []) == 126


def test_wrap_script_signal_death_normalized_to_128_plus_n():
    # A child that kills itself with SIGTERM (15) reports returncode -15; the
    # wrapper normalizes to 128+15 = 143 rather than leaking the negative code.
    cmd = wrap_script("sh", name="s", short_help="shell")
    assert _invoke(cmd, ["-c", "kill -TERM $$"]) == 143


def test_run_root_returns_exit_code_from_version_and_help():
    # --help / --version are clean-exit (click.exceptions.Exit) paths; run_root
    # must surface 0, not leak the exception.
    @click.group(invoke_without_command=True)
    @click.version_option(version="9.9.9", prog_name="x")
    @click.pass_context
    def root(ctx):
        if ctx.invoked_subcommand is None:
            ctx.exit(0)

    assert run_root(root, ["--version"]) == 0
    assert run_root(root, ["--help"]) == 0
    assert run_root(root, []) == 0


def test_stub_group_bare_exits_stub_exit():
    grp = stub_group("g", short_help="stub group", help="A stub group.")
    assert run_root(grp, []) == STUB_EXIT


def test_stub_group_help_flag_exits_0():
    grp = stub_group("g", short_help="stub group", help="A stub group.")
    assert run_root(grp, ["--help"]) == 0


def test_stub_group_with_a_real_subcommand_dispatches_it():
    grp = stub_group("g", short_help="stub group")
    grp.add_command(wrap_verb(lambda a: 0, name="real", short_help="real leaf"))
    # the real subcommand still works; only the BARE form stub-exits.
    assert run_root(grp, ["real"]) == 0
    assert run_root(grp, []) == STUB_EXIT
