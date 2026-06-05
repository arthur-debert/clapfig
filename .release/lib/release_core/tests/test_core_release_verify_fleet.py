"""release_verify_fleet verb — fleet iteration, result aggregation, exit policy.

Fully offline. The gh ref-resolution and EVERY subprocess (`release-core admin
repos list` clone + --paths, detect-kind, release-sync, lefthook) are mocked at
the proc/gh layer —
nothing is cloned or synced. We assert on the table rows, the per-repo
sync/gate columns, the combined-output logs, and the exit-code policy
(0 all-pass, 1 any sync-FAIL/gate-FAIL/missing, 2 setup error, 64 bad usage).
"""

from __future__ import annotations

import subprocess

import pytest
from release_core import gh, proc
from release_core.verbs import release_verify_fleet as rvf

# Captured before any fixture stubs it, so the log test can restore the real one.
_real_write_log = rvf._write_log


def _cp(returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr=stderr)


@pytest.fixture
def env(monkeypatch, tmp_path):
    """All tools 'present', RELEASE_HOME resolvable, USER pinned, cwd in tmp."""
    monkeypatch.setattr(rvf.shutil, "which", lambda _t: "/usr/bin/x")
    monkeypatch.setattr(gh, "git_rev_parse_verify", lambda ref, *, cwd: True)
    monkeypatch.setattr(gh, "git_rev_parse", lambda ref, *, cwd: "0123456789abcdef" * 2)
    monkeypatch.setenv("VERIFY_FLEET_SCRIPT_DIR", str(tmp_path / "bin"))
    monkeypatch.setenv("USER", "tester")
    monkeypatch.chdir(tmp_path)
    # Default: stub log writes (fake roots don't exist on disk). The dedicated
    # log test restores the real _write_log against a real tmp dir.
    monkeypatch.setattr(rvf, "_write_log", lambda path, result: None)
    return tmp_path


class _Driver:
    """Records proc.run calls and returns scripted CompletedProcesses by command.

    `paths` is the `release-core admin repos list --paths` stdout (the TAB lines
    the verb parses); `kinds`/`sync_rc`/`gate_rc` map an abspath → its
    detect-kind / sync / gate result so a test can make individual repos pass or
    fail."""

    # The fleet accessor is now invoked as the hierarchical CLI (`managed-repos`
    # was retired in the B2 cutover; #468). Match on the leading command vector.
    _REPOS_LIST = ["release-core", "admin", "repos", "list"]

    def __init__(self, paths, kinds=None, sync_rc=None, gate_rc=None, clone_rc=0):
        self.paths = paths
        self.kinds = kinds or {}
        self.sync_rc = sync_rc or {}
        self.gate_rc = gate_rc or {}
        self.clone_rc = clone_rc
        self.calls = []

    def __call__(self, cmd, **kw):
        self.calls.append((cmd, kw))
        tool = cmd[0]
        if cmd[: len(self._REPOS_LIST)] == self._REPOS_LIST and "--clone" in cmd:
            return _cp(returncode=self.clone_rc)
        if cmd[: len(self._REPOS_LIST)] == self._REPOS_LIST and "--paths" in cmd:
            return _cp(stdout=self.paths)
        if tool == "detect-kind":
            cwd = kw.get("cwd")
            kind = self.kinds.get(cwd, "rust-cli")
            return _cp(returncode=0 if kind != "?" else 1, stdout="" if kind == "?" else kind)
        if tool == "release-sync":
            cwd = kw.get("cwd")
            return _cp(self.sync_rc.get(cwd, 0), stdout="sync-out\n", stderr="sync-err\n")
        if tool == "lefthook":
            cwd = kw.get("cwd")
            return _cp(self.gate_rc.get(cwd, 0), stdout="gate-out\n", stderr="gate-err\n")
        raise AssertionError(f"unexpected proc.run: {cmd}")


def _row(repo, root):
    return f"{repo}\t{root}/{repo}\tfound"


# ── usage / setup exit codes ──────────────────────────────────────────────────


def test_help_exits_zero_and_prints_usage(env, capsys):
    assert rvf.main(["--help"]) == 0
    assert "release-verify-fleet" in capsys.readouterr().out


def test_unknown_arg_is_usage_error(env, capsys):
    assert rvf.main(["--bogus"]) == 64
    assert "unknown arg: --bogus" in capsys.readouterr().err


def test_missing_flag_value_is_usage_error(env, capsys):
    assert rvf.main(["--ref"]) == 64
    assert "--ref needs a value" in capsys.readouterr().err


def test_missing_tool_exits_2(env, monkeypatch, capsys):
    monkeypatch.setattr(rvf.shutil, "which", lambda t: None if t == "lefthook" else "/usr/bin/x")
    assert rvf.main([]) == 2
    assert "lefthook not on PATH" in capsys.readouterr().err


def test_bad_ref_exits_2(env, monkeypatch, capsys):
    monkeypatch.setattr(gh, "git_rev_parse_verify", lambda ref, *, cwd: False)
    assert rvf.main(["--ref", "nope"]) == 2
    assert "bad --ref 'nope'" in capsys.readouterr().err


# ── happy path / aggregation ──────────────────────────────────────────────────


def test_all_pass_exits_zero_with_table(env, monkeypatch, capsys):
    root = "/fleetroot"
    paths = "\n".join([_row("o/a", root), _row("o/b", root)]) + "\n"
    monkeypatch.setattr(proc, "run", _Driver(paths))
    rc = rvf.main(["--root", root])
    out = capsys.readouterr()
    assert rc == 0
    lines = out.out.splitlines()
    assert lines[0] == "repo\tkind\tsync\tgate"
    assert "o/a\trust-cli\tok\tpass" in lines
    assert "o/b\trust-cli\tok\tpass" in lines
    assert "all consumers pass the gate" in out.err


def test_gate_fail_exits_1(env, monkeypatch, capsys):
    root = "/fleetroot"
    paths = _row("o/a", root) + "\n"
    monkeypatch.setattr(proc, "run", _Driver(paths, gate_rc={f"{root}/o/a": 1}))
    rc = rvf.main(["--root", root])
    out = capsys.readouterr()
    assert rc == 1
    assert "o/a\trust-cli\tok\tFAIL" in out.out.splitlines()
    assert "FAILURES above" in out.err


def test_sync_fail_skips_gate_and_exits_1(env, monkeypatch, capsys):
    root = "/fleetroot"
    paths = _row("o/a", root) + "\n"
    driver = _Driver(paths, sync_rc={f"{root}/o/a": 1})
    monkeypatch.setattr(proc, "run", driver)
    rc = rvf.main(["--root", root])
    assert rc == 1
    assert "o/a\trust-cli\tFAILED\tskipped" in capsys.readouterr().out.splitlines()
    # gate (lefthook) must NOT run when sync failed.
    assert not any(c[0][0] == "lefthook" for c in driver.calls)


def test_missing_clone_row_exits_1(env, monkeypatch, capsys):
    root = "/fleetroot"
    paths = f"o/gone\t{root}/o/gone\tmissing\n"
    monkeypatch.setattr(proc, "run", _Driver(paths))
    rc = rvf.main(["--root", root])
    assert rc == 1
    assert "o/gone\t-\tmissing\tskipped" in capsys.readouterr().out.splitlines()


def test_unknown_kind_renders_question_mark(env, monkeypatch, capsys):
    root = "/fleetroot"
    abspath = f"{root}/o/a"
    paths = _row("o/a", root) + "\n"
    monkeypatch.setattr(proc, "run", _Driver(paths, kinds={abspath: "?"}))
    rvf.main(["--root", root])
    assert "o/a\t?\tok\tpass" in capsys.readouterr().out.splitlines()


def test_paths_line_with_extra_tab_does_not_crash(env, monkeypatch, capsys):
    # Mirror `read -r repo abspath found`: a line with more than three tab fields
    # must NOT raise a ValueError unpack — the third field absorbs the remainder
    # (split maxsplit=2), exactly as bash read does. The trailing tab makes the
    # `found` field != "found", so the repo is reported missing — the faithful
    # bash outcome. The point under test is "no crash", matching read's tolerance.
    root = "/fleetroot"
    paths = "o/a\t/fleetroot/o/a\tfound\textra\n"
    monkeypatch.setattr(proc, "run", _Driver(paths))
    rc = rvf.main(["--root", root])
    assert rc == 1
    assert "o/a\t-\tmissing\tskipped" in capsys.readouterr().out.splitlines()


# ── propagated env / args ─────────────────────────────────────────────────────


def test_only_is_split_and_forwarded_to_both_managed_repos_calls(env, monkeypatch, capsys):
    root = "/fleetroot"
    paths = _row("o/a", root) + "\n"
    driver = _Driver(paths)
    monkeypatch.setattr(proc, "run", driver)
    rvf.main(["--root", root, "--only", "o/a,o/b"])
    mr_calls = [c for c in driver.calls if c[0][: len(_Driver._REPOS_LIST)] == _Driver._REPOS_LIST]
    for cmd, kw in mr_calls:
        assert cmd[-2:] == ["o/a", "o/b"]
        assert kw["env"]["REPOS_ROOT"] == root


def test_refresh_adds_flag_to_clone(env, monkeypatch):
    root = "/fleetroot"
    driver = _Driver(_row("o/a", root) + "\n")
    monkeypatch.setattr(proc, "run", driver)
    rvf.main(["--root", root, "--refresh"])
    clone = next(c for c in driver.calls if "--clone" in c[0])
    assert "--refresh" in clone[0]


def test_sync_pins_ref_sha_and_release_home(env, monkeypatch):
    root = "/fleetroot"
    driver = _Driver(_row("o/a", root) + "\n")
    monkeypatch.setattr(proc, "run", driver)
    rvf.main(["--root", root])
    sync = next(c for c in driver.calls if c[0][0] == "release-sync")
    assert sync[1]["env"]["RELEASE_REF"] == "0123456789abcdef" * 2
    # RELEASE_HOME is the shim dir's parent (VERIFY_FLEET_SCRIPT_DIR/..).
    assert sync[1]["env"]["RELEASE_HOME"] == rvf._release_home()
    assert sync[1]["cwd"] == f"{root}/o/a"


def test_clone_failure_is_non_fatal_warns_and_continues(env, monkeypatch, capsys):
    root = "/fleetroot"
    driver = _Driver(_row("o/a", root) + "\n", clone_rc=1)
    monkeypatch.setattr(proc, "run", driver)
    rc = rvf.main(["--root", root])
    err = capsys.readouterr().err
    assert "fleet clone reported failures" in err
    assert rc == 0  # the one consumer still synced + passed


# ── logs ──────────────────────────────────────────────────────────────────────


def test_writes_combined_sync_and_gate_logs(env, monkeypatch, tmp_path):
    # Restore the real _write_log (the env fixture stubs it) and use a real
    # abspath under tmp so the log files can actually be written.
    monkeypatch.setattr(rvf, "_write_log", _real_write_log)
    repo_dir = tmp_path / "o" / "a"
    repo_dir.mkdir(parents=True)
    root = str(tmp_path)
    paths = f"o/a\t{repo_dir}\tfound\n"
    monkeypatch.setattr(proc, "run", _Driver(paths))
    rvf.main(["--root", root])
    # _write_log concatenates stdout then stderr verbatim (bash `>LOG 2>&1`).
    assert (repo_dir / ".verify-sync.log").read_text() == "sync-out\nsync-err\n"
    assert (repo_dir / ".verify-gate.log").read_text() == "gate-out\ngate-err\n"
