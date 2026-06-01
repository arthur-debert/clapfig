"""release_beta_list verb — branch enumeration, age + ahead-of-main columns,
and the empty/help/usage paths.

Offline: the git boundary (fetch, for-each-ref, rev-list) is stubbed at
proc.run with a recorded result map, never at subprocess.
"""

from __future__ import annotations

import subprocess

import pytest
from release_core import proc
from release_core.verbs import release_beta_list as rbl


def _completed(cmd, *, returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(cmd, returncode, stdout, stderr)


@pytest.fixture
def release_home(tmp_path, monkeypatch):
    (tmp_path / ".git").mkdir()
    monkeypatch.setenv("RELEASE_HOME", str(tmp_path))
    return tmp_path


def test_help_exits_0(capsys):
    assert rbl.main(["--help"]) == 0
    assert "release/beta" in capsys.readouterr().out


def test_unknown_arg_is_usage_error(capsys):
    assert rbl.main(["--bogus"]) == 64
    assert "unknown arg" in capsys.readouterr().err


def test_release_home_not_a_clone_errors(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("RELEASE_HOME", str(tmp_path / "nope"))
    assert rbl.main([]) == 1
    assert "is not a git clone" in capsys.readouterr().err


def test_no_betas_prints_none(release_home, monkeypatch, capsys):
    def fake_run(cmd, **kw):
        if "for-each-ref" in cmd:
            return _completed(cmd, returncode=0, stdout="")
        return _completed(cmd, returncode=0)

    monkeypatch.setattr(proc, "run", fake_run)
    assert rbl.main([]) == 0
    assert capsys.readouterr().out.strip() == "(none)"


def test_lists_betas_with_age_and_ahead(release_home, monkeypatch, capsys):
    feed = "origin/release/beta/foo\t2 days ago\norigin/release/beta/bar\t5 weeks ago\n"

    def fake_run(cmd, **kw):
        if "for-each-ref" in cmd:
            return _completed(cmd, returncode=0, stdout=feed)
        if "rev-list" in cmd:
            # ahead count keyed off the ref in the range arg.
            rng = cmd[-1]
            count = "3" if "foo" in rng else "0"
            return _completed(cmd, returncode=0, stdout=count + "\n")
        return _completed(cmd, returncode=0)

    monkeypatch.setattr(proc, "run", fake_run)
    assert rbl.main([]) == 0
    lines = capsys.readouterr().out.splitlines()
    assert lines[0].split() == ["branch", "age", "ahead-of-main"]
    # short name strips the leading origin/ ; age + ahead columns present.
    assert "release/beta/foo" in lines[2]
    assert lines[2].rstrip().endswith("3")
    assert "release/beta/bar" in lines[3]
    assert lines[3].rstrip().endswith("0")


def test_rev_list_failure_marks_question(release_home, monkeypatch, capsys):
    def fake_run(cmd, **kw):
        if "for-each-ref" in cmd:
            return _completed(cmd, returncode=0, stdout="origin/release/beta/x\t1 hour ago\n")
        if "rev-list" in cmd:
            return _completed(cmd, returncode=128, stdout="", stderr="bad ref")
        return _completed(cmd, returncode=0)

    monkeypatch.setattr(proc, "run", fake_run)
    assert rbl.main([]) == 0
    assert capsys.readouterr().out.splitlines()[2].rstrip().endswith("?")
