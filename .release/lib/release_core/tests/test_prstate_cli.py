"""CLI arg-handling contract (the offline surface — no gh, no network)."""

from __future__ import annotations

from release_core.prstate.cli import task_status


def test_help_exits_zero(capsys):
    assert task_status.main(["--help"]) == 0
    assert "gh-task-status" in capsys.readouterr().out


def test_nonnumeric_pr_is_usage_error(capsys):
    assert task_status.main(["abc"]) == 64
    assert "numeric" in capsys.readouterr().err


def test_unknown_option_is_usage_error(capsys):
    assert task_status.main(["--nope"]) == 64
    assert "unknown option" in capsys.readouterr().err


def test_too_many_args_is_usage_error(capsys):
    assert task_status.main(["1", "2"]) == 64
    assert "too many" in capsys.readouterr().err
