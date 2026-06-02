"""proc — the subprocess runner. Uses only harmless local commands (no network)."""

from __future__ import annotations

import pytest
from release_core import proc
from release_core.proc import ProcError


def test_out_strips():
    assert proc.out(["printf", "hello\n"]) == "hello"


def test_run_returns_completed_process():
    cp = proc.run(["printf", "abc"])
    assert cp.stdout == "abc"
    assert cp.returncode == 0


def test_run_nonzero_raises_proc_error():
    with pytest.raises(ProcError) as exc:
        proc.run(["false"])
    assert exc.value.returncode != 0
    assert exc.value.cmd == ["false"]


def test_run_nonzero_no_check_returns():
    cp = proc.run(["false"], check=False)
    assert cp.returncode != 0


def test_env_is_merged_over_environ(monkeypatch):
    monkeypatch.setenv("PRESERVED", "yes")
    # printenv reads the child env; PRESERVED comes from os.environ, EXTRA from env=.
    assert proc.out(["printenv", "PRESERVED"], env={"EXTRA": "1"}) == "yes"
    assert proc.out(["printenv", "EXTRA"], env={"EXTRA": "1"}) == "1"


def test_input_is_passed_to_stdin():
    assert proc.out(["cat"], input="piped") == "piped"


def test_cwd_is_honoured(tmp_path):
    (tmp_path / "marker").write_text("")
    out = proc.out(["ls"], cwd=str(tmp_path))
    assert "marker" in out
