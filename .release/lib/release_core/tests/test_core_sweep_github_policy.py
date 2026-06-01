"""sweep_github_policy verb — the filesystem subtree copy (create/ok/conflict/force).

Pure filesystem behaviour, exercised over tmp_path trees — no gh, no network.
"""

from __future__ import annotations

import os

from release_core.verbs import sweep_github_policy as sweep


def _write(path: str, text: str, *, mode: int | None = None) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)
    if mode is not None:
        os.chmod(path, mode)


def test_process_subtree_creates_missing(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / ".github" / "CODEOWNERS"), "* @me\n")
    dest.mkdir()
    os.chdir(dest)
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(src), force=False, tally=tally, emit=lines.append)
    assert tally.created == 1
    assert (dest / ".github" / "CODEOWNERS").read_text() == "* @me\n"
    assert lines == ["  created   .github/CODEOWNERS"]


def test_process_subtree_ok_when_identical(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / "f.txt"), "same\n")
    _write(str(dest / "f.txt"), "same\n")
    os.chdir(dest)
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(src), force=False, tally=tally, emit=lines.append)
    assert tally.skipped == 1
    assert lines == ["  ok        f.txt"]


def test_process_subtree_conflict_without_force(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / "f.txt"), "new\n")
    _write(str(dest / "f.txt"), "old\n")
    os.chdir(dest)
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(src), force=False, tally=tally, emit=lines.append)
    assert tally.conflicts == 1
    assert (dest / "f.txt").read_text() == "old\n"  # not overwritten
    assert lines == ["  conflict  f.txt (differs; --force to overwrite)"]


def test_process_subtree_updates_with_force(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / "f.txt"), "new\n")
    _write(str(dest / "f.txt"), "old\n")
    os.chdir(dest)
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(src), force=True, tally=tally, emit=lines.append)
    assert tally.updated == 1
    assert (dest / "f.txt").read_text() == "new\n"
    assert lines == ["  updated   f.txt"]


def test_process_subtree_preserves_executable_bit(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / "run.sh"), "#!/bin/sh\n", mode=0o755)
    dest.mkdir()
    os.chdir(dest)
    tally = sweep.Tally()
    sweep.process_subtree(str(src), force=False, tally=tally, emit=lambda _l: None)
    assert os.access(dest / "run.sh", os.X_OK)


def test_process_subtree_skips_dotds_store_and_sorts(tmp_path):
    src = tmp_path / "src"
    dest = tmp_path / "dest"
    _write(str(src / "b.txt"), "b\n")
    _write(str(src / "a.txt"), "a\n")
    _write(str(src / ".DS_Store"), "junk")
    dest.mkdir()
    os.chdir(dest)
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(src), force=False, tally=tally, emit=lines.append)
    assert lines == ["  created   a.txt", "  created   b.txt"]  # sorted, no .DS_Store
    assert tally.created == 2


def test_process_subtree_missing_prefix_is_noop(tmp_path):
    tally = sweep.Tally()
    lines: list[str] = []
    sweep.process_subtree(str(tmp_path / "nope"), force=False, tally=tally, emit=lines.append)
    assert lines == []
    assert tally == sweep.Tally()


def test_main_undetected_kind_exits_1_cleanly(tmp_path, monkeypatch, capsys):
    """When no --stack is given and manifest.detect_kind can't determine the
    kind, main must print the bash's "could not detect kind of ..." line and
    return 1 — not crash with an uncaught KindError. Regression for PR #392."""
    from release_core import manifest

    monkeypatch.setattr(sweep.proc, "out", lambda *a, **k: str(tmp_path))
    monkeypatch.setattr(sweep.os, "chdir", lambda _p: None)

    def boom(_root):
        raise manifest.KindError(f"could not detect kind of {tmp_path}")

    monkeypatch.setattr(sweep.manifest, "detect_kind", boom)

    rc = sweep.main([])
    assert rc == 1
    err = capsys.readouterr().err
    assert "could not detect kind of" in err
