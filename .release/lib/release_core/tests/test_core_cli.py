"""cli.parse — the shared arg harness."""

from __future__ import annotations

import pytest
from release_core import cli
from release_core.cli import Opt


def test_boolean_flag_present():
    vals, pos = cli.parse(["--json"], [Opt("--json")])
    assert vals == {"json": True}
    assert pos == []


def test_boolean_flag_absent_default_false():
    vals, _ = cli.parse([], [Opt("--json", default=False)])
    assert vals["json"] is False


def test_value_option_space_form():
    vals, _ = cli.parse(["--repo", "owner/x"], [Opt("--repo", takes_value=True)])
    assert vals["repo"] == "owner/x"


def test_value_option_equals_form():
    vals, _ = cli.parse(["--repo=owner/x"], [Opt("--repo", takes_value=True)])
    assert vals["repo"] == "owner/x"


def test_value_option_default():
    vals, _ = cli.parse([], [Opt("--repo", takes_value=True, default="self")])
    assert vals["repo"] == "self"


def test_positionals_collected():
    vals, pos = cli.parse(["a", "b"], [], positionals=(0, 2))
    assert pos == ["a", "b"]


def test_mixed_options_and_positionals():
    vals, pos = cli.parse(
        ["--json", "dir1"],
        [Opt("--json")],
        positionals=(1, 1),
    )
    assert vals["json"] is True
    assert pos == ["dir1"]


def test_double_dash_treats_rest_as_positional():
    vals, pos = cli.parse(
        ["--", "--not-an-option"],
        [Opt("--json", default=False)],
        positionals=(1, 1),
    )
    assert pos == ["--not-an-option"]
    assert vals["json"] is False


def test_help_long_exits_0(capsys):
    with pytest.raises(SystemExit) as exc:
        cli.parse(["--help"], [], doc="USAGE: thing")
    assert exc.value.code == cli.EXIT_OK
    assert "USAGE: thing" in capsys.readouterr().out


def test_help_short_exits_0():
    with pytest.raises(SystemExit) as exc:
        cli.parse(["-h"], [], doc="x")
    assert exc.value.code == cli.EXIT_OK


def test_unknown_option_exits_64(capsys):
    with pytest.raises(SystemExit) as exc:
        cli.parse(["--nope"], [Opt("--json")])
    assert exc.value.code == cli.EXIT_USAGE
    assert "unknown option" in capsys.readouterr().err


def test_missing_value_exits_64():
    with pytest.raises(SystemExit) as exc:
        cli.parse(["--repo"], [Opt("--repo", takes_value=True)])
    assert exc.value.code == cli.EXIT_USAGE


def test_value_to_boolean_flag_exits_64():
    with pytest.raises(SystemExit) as exc:
        cli.parse(["--json=yes"], [Opt("--json")])
    assert exc.value.code == cli.EXIT_USAGE


def test_too_many_positionals_exits_64():
    with pytest.raises(SystemExit) as exc:
        cli.parse(["a", "b"], [], positionals=(0, 1))
    assert exc.value.code == cli.EXIT_USAGE


def test_too_few_positionals_exits_64():
    with pytest.raises(SystemExit) as exc:
        cli.parse([], [], positionals=(1, 1))
    assert exc.value.code == cli.EXIT_USAGE
