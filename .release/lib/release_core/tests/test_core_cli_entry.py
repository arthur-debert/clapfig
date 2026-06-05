"""cli_entry (cli_entry.py): the top-level ``release-core`` click assembler.

The dispatcher is now a click ``Group`` tree (release#460), so this pins the
NEW contract: ``main(argv) -> int`` is preserved as the entrypoint; the root
renders the grouped help; bare invocation is a discovery entry (prints help,
exit 0); an unknown command is a click usage error (exit 2); ``init`` and
``selfcheck`` are folded in as top-level commands and forward their argv
(incl. ``--help``) verbatim to the underlying verb.

The verb-wrap / script-wrap *helpers* themselves (the patterns every leaf uses)
are unit-tested in test_core_cli_helpers.py; the group TREE (which leaves exist,
short-help one-liners) in test_core_cli_tree.py.
"""

from __future__ import annotations

from release_core import cli_entry


def test_no_args_prints_help_exit_0(capsys):
    rc = cli_entry.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-core" in out
    assert "Usage:" in out
    assert "Commands:" in out


def test_help_flag_prints_help_exit_0(capsys):
    for flag in ("-h", "--help"):
        rc = cli_entry.main([flag])
        out = capsys.readouterr().out
        assert rc == 0
        assert "Usage:" in out
        assert "Commands:" in out


def test_root_help_lists_the_group_tree(capsys):
    cli_entry.main(["--help"])
    out = capsys.readouterr().out
    # Top-level groups + a couple of per-project commands are all visible.
    for token in ("pr", "ci", "admin", "init", "cut", "status"):
        assert token in out


def test_version_flag(capsys):
    rc = cli_entry.main(["--version"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-core" in out
    assert "version" in out


def test_unknown_command_is_usage_error_exit_2(capsys):
    rc = cli_entry.main(["bogus"])
    err = capsys.readouterr().err
    assert rc == 2
    assert "No such command" in err
    assert "bogus" in err


def test_init_help_routes_to_init_verb(capsys):
    # `release-core init --help` reaches the init verb's own --help (exit 0),
    # byte-identical to the verb's own docstring help.
    rc = cli_entry.main(["init", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-core init" in out


def test_selfcheck_dispatches_and_returns_its_exit_code():
    # selfcheck is a real, side-effect-free verb (checks deps are importable);
    # in this venv click+stdlib are present, so it returns 0. Proves the folded
    # selfcheck leaf dispatches to the verb and returns its exit code verbatim.
    assert cli_entry.main(["selfcheck"]) == 0
