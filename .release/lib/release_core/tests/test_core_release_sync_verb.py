"""release-sync verb (verbs/release_sync.py): the CLI-boundary behavior that the
pure engine (test_core_sync.py) does not cover.

Pins two review-surfaced contracts:
  - the managed CLAUDE.md write lands at umask-respecting 0o644, NOT the 0o600
    that tempfile.mkstemp hands back (the apply phase chmods before os.replace);
  - a malformed .release-sync.yaml / manifest.yaml is caught at the CLI boundary
    and exits non-zero with a clean message, never a YamlError traceback.
"""

from __future__ import annotations

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


def test_resolve_capabilities_yamlerror_returns_1_not_traceback(tmp_path, monkeypatch, capsys):
    # Drive main() to the capability-resolution step and have resolve_capabilities
    # raise YamlError (as it does on malformed YAML). main must catch → exit 1 with
    # a clean stderr line, not let the traceback escape.
    from release_core import yamlio

    rh = tmp_path / "release_home"
    (rh / ".git").mkdir(parents=True)
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
