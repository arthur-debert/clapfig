"""cli_entry (cli_entry.py): the top-level `release-core` subcommand dispatcher
(pip-bootstrap PoC §2).

Pins the dispatch contract: subcommand routing + arg forwarding, the no-args /
--help usage block (exit 0), unknown-subcommand usage error (exit 64), and that
the subcommand's own exit code is returned verbatim. The init subcommand's
behavior is covered by test_core_init.py; here the init handler is monkeypatched
to a spy so the test stays a pure dispatch test.
"""

from __future__ import annotations

from release_core import cli_entry


def test_no_args_prints_usage_exit_0(capsys):
    rc = cli_entry.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-core" in out
    assert "Usage:" in out
    assert "init" in out


def test_help_flag_prints_usage_exit_0(capsys):
    for flag in ("-h", "--help"):
        rc = cli_entry.main([flag])
        out = capsys.readouterr().out
        assert rc == 0
        assert "Usage:" in out


def test_dispatches_init_and_forwards_args(monkeypatch):
    seen = {}

    def spy(argv):
        seen["argv"] = argv
        return 0

    monkeypatch.setitem(cli_entry._SUBCOMMANDS, "init", spy)
    rc = cli_entry.main(["init", "--force", "--dry-run"])
    assert rc == 0
    assert seen["argv"] == ["--force", "--dry-run"]


def test_returns_subcommand_exit_code(monkeypatch):
    monkeypatch.setitem(cli_entry._SUBCOMMANDS, "init", lambda argv: 7)
    assert cli_entry.main(["init"]) == 7


def test_unknown_subcommand_is_usage_error_exit_64(capsys):
    rc = cli_entry.main(["bogus"])
    err = capsys.readouterr().err
    assert rc == 64
    assert "unknown subcommand" in err
    assert "bogus" in err


def test_argv_none_reads_sys_argv(monkeypatch, capsys):
    # main(None) must fall back to sys.argv[1:]; simulate `release-core` no-args.
    monkeypatch.setattr(cli_entry.sys, "argv", ["release-core"])
    rc = cli_entry.main(None)
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out


def test_init_help_routes_to_init_verb(capsys):
    # `release-core init --help` reaches the init verb's own --help (exit 0).
    rc = cli_entry.main(["init", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-core init" in out
