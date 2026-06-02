"""release-drift-check: the drift-vs-staleness gate (ADR-0002, release#301).

Fixture-driven, no network/gh/subprocess. The rebuild (release_sync.main) and
the YAML read (yamlio.load) are monkeypatched at the data layer so the tests
exercise pure classification: marker parsing, the in-process --check capture,
the .release-sync.yaml extra-key check, exit codes, and the report text.

These pin the byte-for-byte contract a CI gate depends on — exit codes are
load-bearing (0 clean / 1 drift / 2 internal / 64 usage).
"""

from __future__ import annotations

import pytest
from release_core import yamlio
from release_core.verbs import release_drift_check as drift


@pytest.fixture
def consumer(tmp_path, monkeypatch):
    """A synthetic consumer repo cwd with release-sync + yq 'on PATH' and the
    git repo-root resolution stubbed to tmp_path. Returns the repo root."""
    monkeypatch.setattr(drift.shutil, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(drift.gh, "git", lambda args: str(tmp_path))
    monkeypatch.chdir(tmp_path)
    return tmp_path


def _write_marker(root, sha):
    rel = root / ".release"
    rel.mkdir(exist_ok=True)
    (rel / ".release-sync-source").write_text(
        f"# release-sync provenance\n# informational, not state (ADR-0002)\n{sha}\n",
        encoding="utf-8",
    )


SHA = "a" * 40


# ── Arg parsing / usage ───────────────────────────────────────────────────────


def test_help_prints_usage_exit_0(capsys):
    rc = drift.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-drift-check" in out
    assert "Exit codes:" in out


def test_unknown_arg_exit_64(capsys):
    rc = drift.main(["--bogus"])
    err = capsys.readouterr().err
    assert rc == 64
    assert "unknown arg: --bogus" in err


# ── Guards ────────────────────────────────────────────────────────────────────


def test_missing_release_sync_exit_2(monkeypatch, capsys):
    monkeypatch.setattr(
        drift.shutil, "which", lambda name: None if name == "release-sync" else "/usr/bin/yq"
    )
    rc = drift.main([])
    assert rc == 2
    assert "release-sync not on PATH" in capsys.readouterr().err


def test_missing_yq_exit_2(monkeypatch, capsys):
    monkeypatch.setattr(
        drift.shutil, "which", lambda name: None if name == "yq" else "/usr/bin/release-sync"
    )
    rc = drift.main([])
    assert rc == 2
    assert "yq is required" in capsys.readouterr().err


def test_not_in_git_repo_exit_2(monkeypatch, capsys):
    monkeypatch.setattr(drift.shutil, "which", lambda name: f"/usr/bin/{name}")

    def _boom(args):
        raise RuntimeError("not a git repo")

    monkeypatch.setattr(drift.gh, "git", _boom)
    rc = drift.main([])
    assert rc == 2
    assert "not inside a git repo" in capsys.readouterr().err


# ── No marker → skip (the lazy-backfill no-op) ────────────────────────────────


def test_no_marker_skips_exit_0(consumer, capsys):
    rc = drift.main([])
    assert rc == 0
    assert "skipping drift gate" in capsys.readouterr().out


def test_no_marker_quiet_is_silent(consumer, capsys):
    rc = drift.main(["--quiet"])
    assert rc == 0
    assert capsys.readouterr().out == ""


def test_marker_without_sha_line_exit_2(consumer, capsys):
    (consumer / ".release").mkdir()
    (consumer / ".release" / ".release-sync-source").write_text("no sha here\n")
    rc = drift.main([])
    assert rc == 2
    assert "no 40-char SHA line" in capsys.readouterr().err


# ── Clean rebuild → exit 0 ────────────────────────────────────────────────────


def test_clean_when_rebuild_matches(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("no changes", True))
    rc = drift.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "clean — managed surface matches release@aaaaaaaaaaaa" in out


def test_clean_quiet_is_silent(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("", True))
    rc = drift.main(["--quiet"])
    assert rc == 0
    assert capsys.readouterr().out == ""


# ── Managed-file drift → exit 1 ───────────────────────────────────────────────


def test_managed_drift_exit_1(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("  ~file     lefthook.yml", False))
    rc = drift.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "DRIFT DETECTED" in err
    assert "UPSTREAM" in err
    assert "Managed-file drift (rebuilt against release@aaaaaaaaaaaa)" in err
    assert "~file     lefthook.yml" in err


# ── .release-sync.yaml over-override ──────────────────────────────────────────


def test_extra_sync_keys_drift_exit_1(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("", True))
    (consumer / ".release-sync.yaml").write_text("capabilities: []\nextra-knob: true\n")
    monkeypatch.setattr(yamlio, "load", lambda p: {"capabilities": [], "extra-knob": True})
    rc = drift.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "extra-knob" in err
    assert "beyond `capabilities`" in err


def test_capabilities_only_yaml_is_clean(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("", True))
    (consumer / ".release-sync.yaml").write_text("capabilities: []\n")
    monkeypatch.setattr(yamlio, "load", lambda p: {"capabilities": []})
    rc = drift.main([])
    assert rc == 0
    assert "clean" in capsys.readouterr().out


def test_empty_yaml_is_clean(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("", True))
    (consumer / ".release-sync.yaml").write_text("\n")
    monkeypatch.setattr(yamlio, "load", lambda p: None)  # `(. // {})` → no keys
    rc = drift.main([])
    assert rc == 0


def test_malformed_yaml_hard_fails_exit_2(consumer, monkeypatch, capsys):
    _write_marker(consumer, SHA)
    monkeypatch.setattr(drift, "_run_sync_check", lambda sha: ("", True))
    (consumer / ".release-sync.yaml").write_text("this: [unclosed\n")

    def _boom(p):
        raise yamlio.YamlError("yq … failed (1): bad indentation")

    monkeypatch.setattr(yamlio, "load", _boom)
    rc = drift.main([])
    assert rc == 2
    assert "bad indentation" in capsys.readouterr().err


# ── Marker parsing helper ─────────────────────────────────────────────────────


def test_read_marker_sha_skips_comments_and_crs(tmp_path):
    p = tmp_path / "marker"
    p.write_text(f"# comment\r\n{SHA}\r\n", encoding="utf-8")
    assert drift._read_marker_sha(str(p)) == SHA


def test_read_marker_sha_none_when_absent(tmp_path):
    p = tmp_path / "marker"
    p.write_text("# only comments\nnot-a-sha\n", encoding="utf-8")
    assert drift._read_marker_sha(str(p)) is None


def test_read_marker_sha_rejects_short_hex(tmp_path):
    p = tmp_path / "marker"
    p.write_text("abc123\n", encoding="utf-8")
    assert drift._read_marker_sha(str(p)) is None


# ── _run_sync_check: in-process capture + RELEASE_REF isolation ───────────────


def test_run_sync_check_captures_combined_output(monkeypatch):
    """release_sync.main is dispatched in-process; stdout+stderr are merged and
    trailing newlines stripped (the bash `$(... 2>&1)` semantics)."""
    seen = {}

    def fake_main(argv):
        import os
        import sys

        seen["argv"] = argv
        seen["ref"] = os.environ.get("RELEASE_REF")
        print("on stdout")
        print("on stderr", file=sys.stderr)
        return 1

    monkeypatch.setattr(drift.release_sync, "main", fake_main)
    out, clean = drift._run_sync_check(SHA)
    assert seen["argv"] == ["--check"]
    assert seen["ref"] == SHA  # RELEASE_REF pinned to the marker SHA for the call
    assert clean is False
    assert "on stdout" in out
    assert "on stderr" in out
    assert not out.endswith("\n")


def test_run_sync_check_restores_release_ref(monkeypatch):
    monkeypatch.setenv("RELEASE_REF", "preexisting")
    monkeypatch.setattr(drift.release_sync, "main", lambda argv: 0)
    drift._run_sync_check(SHA)
    import os

    assert os.environ["RELEASE_REF"] == "preexisting"


def test_run_sync_check_unsets_release_ref_when_absent(monkeypatch):
    monkeypatch.delenv("RELEASE_REF", raising=False)
    monkeypatch.setattr(drift.release_sync, "main", lambda argv: 0)
    drift._run_sync_check(SHA)
    import os

    assert "RELEASE_REF" not in os.environ


def test_run_sync_check_treats_rebuild_exception_as_drift(monkeypatch):
    def boom(argv):
        raise RuntimeError("recorded SHA unreachable")

    monkeypatch.setattr(drift.release_sync, "main", boom)
    out, clean = drift._run_sync_check(SHA)
    assert clean is False
    assert "recorded SHA unreachable" in out


def test_run_sync_check_clean_passthrough(monkeypatch):
    monkeypatch.setattr(drift.release_sync, "main", lambda argv: 0)
    out, clean = drift._run_sync_check(SHA)
    assert clean is True
