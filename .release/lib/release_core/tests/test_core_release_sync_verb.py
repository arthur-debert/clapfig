"""release-sync verb (verbs/release_sync.py): the CLI-boundary behavior that the
pure engine (test_core_sync.py) does not cover.

Pins two review-surfaced contracts:
  - the managed CLAUDE.md write lands at umask-respecting 0o644, NOT the 0o600
    that tempfile.mkstemp hands back (the apply phase chmods before os.replace);
  - a malformed .release-sync.yaml / manifest.yaml is caught at the CLI boundary
    and exits non-zero with a clean message, never a YamlError traceback.
"""

from __future__ import annotations

import os
import stat

from release_core import sync
from release_core.verbs import release_sync


def test_apply_claude_write_is_0o644_not_0o600(tmp_path, monkeypatch):
    # tempfile.mkstemp creates the temp at 0o600; the apply phase must chmod to
    # 0o644 before os.replace so the materialized CLAUDE.md is world-readable.
    monkeypatch.chdir(tmp_path)
    claude = sync.ClaudeDecision(action="create", desired="# hello\n")
    release_sync._apply(sync.MirrorPlan(), claude)
    out = tmp_path / sync.CLAUDE_FILE
    assert out.read_text() == "# hello\n"
    mode = stat.S_IMODE(out.stat().st_mode)
    assert mode == 0o644, f"expected 0o644, got {oct(mode)}"


def test_apply_replaces_real_skill_file_with_symlink(tmp_path, monkeypatch):
    """The lex pr-review-respond regression: a pre-existing REAL skill file at a
    managed dest is removed and replaced by the managed symlink (apply phase)."""
    monkeypatch.chdir(tmp_path)
    dest = ".claude/skills/pr-review-respond/SKILL.md"
    real = tmp_path / dest
    real.parent.mkdir(parents=True)
    real.write_text("# stale 157-line hand-copy\n")
    assert not (tmp_path / dest).is_symlink()

    target = sync.link_target(dest)
    mp = sync.MirrorPlan(
        migrated=[dest],
        symlinks_to_create=[f"{dest} -> {target}"],
    )
    release_sync._apply(mp, sync.ClaudeDecision(action="none"))

    link = tmp_path / dest
    assert link.is_symlink()
    assert os.readlink(str(link)) == target


def test_rm_f_removes_real_directory(tmp_path):
    """_rm_f handles a real directory at a managed dest (rm -rf), so a managed
    symlink can take its place — not just files/symlinks."""
    d = tmp_path / "stale-dir"
    d.mkdir()
    (d / "inner.txt").write_text("x\n")
    release_sync._rm_f(str(d))
    assert not d.exists()


def test_apply_replaces_symlinked_skill_root_without_touching_target(tmp_path, monkeypatch):
    """A symlinked skill ROOT is removed and rebuilt as a real dir of managed
    symlinks — the apply must NOT write through the old symlink into its target."""
    monkeypatch.chdir(tmp_path)
    external = tmp_path / "external-target"
    external.mkdir()
    guarded = external / "SKILL.md"
    guarded.write_text("# original target content — must survive\n")

    skills = tmp_path / ".claude" / "skills"
    skills.mkdir(parents=True)
    os.symlink(str(external), str(skills / "lex-primer"))

    dest = ".claude/skills/lex-primer/SKILL.md"
    target = sync.link_target(dest)
    mp = sync.MirrorPlan(
        migrated=[".claude/skills/lex-primer"],
        symlinks_to_create=[f"{dest} -> {target}"],
    )
    release_sync._apply(mp, sync.ClaudeDecision(action="none"))

    # Consumer path is now a real symlink into .release/, and the external target
    # file was never deleted or overwritten.
    link = tmp_path / dest
    assert link.is_symlink()
    assert os.readlink(str(link)) == target
    assert not (skills / "lex-primer").is_symlink()  # root rebuilt as a real dir
    assert guarded.read_text() == "# original target content — must survive\n"


def test_rm_f_tolerates_absent_path(tmp_path):
    """_rm_f ignores absence (rm -f semantics) — covers the TOCTOU window where a
    dir vanishes between the isdir() check and the removal."""
    release_sync._rm_f(str(tmp_path / "never-existed"))  # no raise
    release_sync._rm_f(str(tmp_path / "gone" / "child"))  # no raise


def test_resolve_capabilities_yamlerror_returns_1_not_traceback(tmp_path, monkeypatch, capsys):
    # Drive main() to the capability-resolution step and have resolve_capabilities
    # raise YamlError (as it does on malformed YAML). main must catch → exit 1 with
    # a clean stderr line, not let the traceback escape.
    from release_core import yamlio

    rh = tmp_path / "release_home"
    (rh / ".git").mkdir(parents=True)
    # The guard now probes git (is_git_worktree) instead of os.path.isdir(.git),
    # so this fake clone is reported as a work tree without a real `git init`.
    monkeypatch.setattr(release_sync.gh, "is_git_worktree", lambda path: True)
    monkeypatch.setenv("RELEASE_HOME", str(rh))

    consumer = tmp_path / "consumer"
    consumer.mkdir()
    monkeypatch.chdir(consumer)

    monkeypatch.setattr(release_sync.shutil, "which", lambda name: f"/usr/bin/{name}")
    # gh.git("rev-parse --show-toplevel") → the consumer dir (distinct from RELEASE_HOME).
    monkeypatch.setattr(release_sync.gh, "git", lambda args: str(consumer))
    monkeypatch.setattr(release_sync.manifest, "detect_kind", lambda root: "docs-site")
    monkeypatch.setattr(release_sync.sync, "select_ref", lambda *a, **k: "origin/main")
    monkeypatch.setattr(release_sync.gh, "git_rev_parse", lambda *a, **k: "a" * 40)
    monkeypatch.setattr(release_sync.sync, "_has_nonempty_line", lambda text: True)
    monkeypatch.setattr(release_sync.gh, "git_ls_tree", lambda *a, **k: "templates/docs-site")

    def _raise(*a, **k):
        raise yamlio.YamlError("yq -o=json . failed (1): bad YAML")

    monkeypatch.setattr(release_sync.sync, "resolve_capabilities", _raise)

    rc = release_sync.main([])
    assert rc == 1
    err = capsys.readouterr().err
    assert "release-sync:" in err
    assert "bad YAML" in err
