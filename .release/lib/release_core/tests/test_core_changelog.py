"""test_core_changelog — unit tests for release_core.verbs.changelog.

Filesystem/markdown logic, fixture-driven, no network/gh. Each test runs in an
isolated tmp_path that is chdir'd into and git-init'd (the verbs resolve their
root via cwd walk-up → git toplevel, exactly like the bash they replace).
"""

from __future__ import annotations

import subprocess

import pytest
from release_core.verbs import changelog


@pytest.fixture
def repo(tmp_path, monkeypatch):
    """A git repo rooted at tmp_path, cwd set into it."""
    monkeypatch.chdir(tmp_path)
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    return tmp_path


# --- semver validation parity ----------------------------------------------


@pytest.mark.parametrize(
    "v,valid",
    [
        ("1.0.0", True),
        ("0.5.0-rc.1", True),
        ("2.0.0-alpha.10", True),
        ("1.2.3+build.5", True),
        ("1.0", False),  # not three components
        ("01.0.0", False),  # leading zero (semver-tool NAT rejects)
        ("1.0.0-01", False),  # numeric prerelease ident with leading zero
        ("v1.0.0", False),  # semver-tool regex starts at NAT — no 'v'; callers also pre-reject
        ("", False),
    ],
)
def test_is_valid_semver_matches_semver_tool(v, valid):
    assert changelog._is_valid_semver(v) is valid


# --- changelog-add ----------------------------------------------------------


def test_add_inline_args_join_with_space(repo):
    rc = changelog.add_main(["pr-142", "- Fix", "crash"])
    assert rc == 0
    frag = repo / "CHANGELOG" / "unreleased-pr-142.md"
    assert frag.read_bytes() == b"- Fix crash\n"


def test_add_numeric_slug_prefixed(repo):
    changelog.add_main(["142", "- x"])
    assert (repo / "CHANGELOG" / "unreleased-pr-142.md").exists()
    assert not (repo / "CHANGELOG" / "unreleased-142.md").exists()


def test_add_stdin_bytes_verbatim(repo, monkeypatch):
    import io

    payload = b"- one\n- two\n\n"
    monkeypatch.setattr("sys.stdin", type("S", (), {"buffer": io.BytesIO(payload)})())
    changelog.add_main(["multi"])
    assert (repo / "CHANGELOG" / "unreleased-multi.md").read_bytes() == payload


def test_add_collision_fails_without_force(repo, capsys):
    changelog.add_main(["142", "first"])
    rc = changelog.add_main(["142", "second"])
    assert rc == 1
    assert "already exists" in capsys.readouterr().err
    assert (repo / "CHANGELOG" / "unreleased-pr-142.md").read_text() == "first\n"


def test_add_force_overwrites(repo):
    changelog.add_main(["142", "first"])
    rc = changelog.add_main(["--force", "142", "second"])
    assert rc == 0
    assert (repo / "CHANGELOG" / "unreleased-pr-142.md").read_text() == "second\n"


@pytest.mark.parametrize("slug", ["../evil", ".hidden", "a/b"])
def test_add_rejects_unsafe_slug(repo, slug, capsys):
    rc = changelog.add_main([slug, "x"])
    assert rc == 2
    assert "slug must match" in capsys.readouterr().err


def test_add_missing_slug_usage(repo, capsys):
    rc = changelog.add_main([])
    assert rc == 2
    assert "usage:" in capsys.readouterr().err


def test_add_outside_git_and_no_changelog(tmp_path, monkeypatch, capsys):
    monkeypatch.chdir(tmp_path)  # no git init, no CHANGELOG/
    rc = changelog.add_main(["142", "x"])
    assert rc == 1
    assert "no CHANGELOG/ found" in capsys.readouterr().err


# --- changelog-cut ----------------------------------------------------------


def _frag(repo, name, body):
    d = repo / "CHANGELOG"
    d.mkdir(exist_ok=True)
    (d / name).write_bytes(body)


def test_cut_concatenates_with_header_and_deletes(repo):
    _frag(repo, "unreleased-a.md", b"- one\n")
    _frag(repo, "unreleased-b.md", b"- two\n")
    rc = changelog.cut_main(["0.1.0"])
    assert rc == 0
    out = (repo / "CHANGELOG" / "0.1.0.md").read_text()
    assert out.startswith("## 0.1.0 - ")
    assert "- one\n- two\n" in out
    assert not list((repo / "CHANGELOG").glob("unreleased-*.md"))


def test_cut_appends_newline_to_unterminated_fragment(repo):
    _frag(repo, "unreleased-a.md", b"- first")
    _frag(repo, "unreleased-b.md", b"- second")
    changelog.cut_main(["0.1.0"])
    body = (repo / "CHANGELOG" / "0.1.0.md").read_text()
    assert "- first\n- second\n" in body


def test_cut_byte_sort_order(repo):
    _frag(repo, "unreleased-c.md", b"- c\n")
    _frag(repo, "unreleased-a.md", b"- a\n")
    _frag(repo, "unreleased-b.md", b"- b\n")
    changelog.cut_main(["0.1.0"])
    body = (repo / "CHANGELOG" / "0.1.0.md").read_text()
    assert body.index("- a") < body.index("- b") < body.index("- c")


def test_cut_no_fragments_fails(repo, capsys):
    (repo / "CHANGELOG").mkdir()
    rc = changelog.cut_main(["0.1.0"])
    assert rc == 1
    assert "no CHANGELOG/unreleased-*.md fragments" in capsys.readouterr().err


def test_cut_refuses_overwrite(repo, capsys):
    _frag(repo, "unreleased-a.md", b"- one\n")
    changelog.cut_main(["0.1.0"])
    _frag(repo, "unreleased-b.md", b"- two\n")
    rc = changelog.cut_main(["0.1.0"])
    assert rc == 1
    assert "already exists" in capsys.readouterr().err
    assert (repo / "CHANGELOG" / "unreleased-b.md").exists()


@pytest.mark.parametrize("v", ["1.0", "01.0.0", "1.2.x"])
def test_cut_rejects_non_semver(repo, v, capsys):
    _frag(repo, "unreleased-a.md", b"- one\n")
    rc = changelog.cut_main([v])
    assert rc == 2
    assert "must be valid semver" in capsys.readouterr().err


@pytest.mark.parametrize("v", ["v1.2.3", "V1.2.3"])
def test_cut_rejects_v_prefix(repo, v, capsys):
    _frag(repo, "unreleased-a.md", b"- one\n")
    rc = changelog.cut_main([v])
    assert rc == 2
    assert "bare semver" in capsys.readouterr().err
    assert (repo / "CHANGELOG" / "unreleased-a.md").exists()


def test_cut_accepts_prerelease(repo):
    _frag(repo, "unreleased-a.md", b"- rc\n")
    rc = changelog.cut_main(["2.0.0-rc.1"])
    assert rc == 0
    assert (repo / "CHANGELOG" / "2.0.0-rc.1.md").exists()


def test_cut_missing_version_usage(repo, capsys):
    rc = changelog.cut_main([])
    assert rc == 2
    assert "usage:" in capsys.readouterr().err


# --- changelog-render -------------------------------------------------------


def test_render_no_changelog_dir_fails(repo, capsys):
    rc = changelog.render_main([])
    assert rc == 1
    assert "CHANGELOG/ directory not found" in capsys.readouterr().err


def test_render_empty_changelog(repo):
    (repo / "CHANGELOG").mkdir()
    rc = changelog.render_main([])
    assert rc == 0
    text = (repo / "CHANGELOG.md").read_text()
    assert text == (
        "<!-- generated - do not edit. See CHANGELOG/README.txt -->\n\n"
        "# Changelog\n\n"
        "## Unreleased\n\n"
    )


def test_render_output_mode_respects_umask(repo):
    """render writes via mkstemp (0o600) + os.replace; the final CHANGELOG.md
    must carry umask-default perms (e.g. 0o644), not the 0o600 mkstemp leaks —
    parity with the bash `mktemp + mv`. Regression for PR #392 review."""
    import os
    import stat

    (repo / "CHANGELOG").mkdir()
    old = os.umask(0o022)
    try:
        rc = changelog.render_main([])
    finally:
        os.umask(old)
    assert rc == 0
    mode = stat.S_IMODE(os.stat(repo / "CHANGELOG.md").st_mode)
    assert mode == 0o644, oct(mode)


def test_render_descending_semver_order(repo):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "1.0.0.md").write_text("## 1.0.0\n\n- a\n")
    (d / "0.4.2.md").write_text("## 0.4.2\n\n- b\n")
    (d / "0.5.0.md").write_text("## 0.5.0\n\n- c\n")
    (d / "1.2.10.md").write_text("## 1.2.10\n\n- d\n")
    (d / "1.2.2.md").write_text("## 1.2.2\n\n- e\n")
    changelog.render_main([])
    text = (repo / "CHANGELOG.md").read_text()
    order = [
        text.index("## 1.2.10"),
        text.index("## 1.2.2"),
        text.index("## 1.0.0"),
        text.index("## 0.5.0"),
        text.index("## 0.4.2"),
    ]
    assert order == sorted(order)


def test_render_bare_release_above_prereleases(repo):
    d = repo / "CHANGELOG"
    d.mkdir()
    for stem in ["0.5.0", "0.5.0-rc.1", "0.5.0-alpha.10", "0.5.0-alpha.2"]:
        (d / f"{stem}.md").write_text(f"## {stem}\n\n- x\n")
    changelog.render_main([])
    text = (repo / "CHANGELOG.md").read_text()
    assert (
        text.index("## 0.5.0\n")
        < text.index("## 0.5.0-rc.1")
        < text.index("## 0.5.0-alpha.10")
        < text.index("## 0.5.0-alpha.2")
    )


def test_render_legacy_appended_verbatim(repo):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "1.0.0.md").write_text("## 1.0.0\n\n- a\n")
    (d / "legacy.md").write_text("old stuff\n\n## 0.0.1\n- ancient\n")
    changelog.render_main([])
    text = (repo / "CHANGELOG.md").read_text()
    assert text.endswith("old stuff\n\n## 0.0.1\n- ancient\n")


def test_render_idempotent(repo):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "1.0.0.md").write_text("## 1.0.0\n\n- a\n")
    (d / "unreleased-x.md").write_text("- x\n")
    changelog.render_main([])
    first = (repo / "CHANGELOG.md").read_bytes()
    changelog.render_main([])
    assert (repo / "CHANGELOG.md").read_bytes() == first


def test_render_unparseable_filename_fails_loud(repo, capsys):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "not-a-version.md").write_text("x\n")
    rc = changelog.render_main([])
    assert rc == 1
    err = capsys.readouterr().err
    assert "unparseable version filename" in err
    assert "not-a-version.md" in err
    assert not list(repo.glob("CHANGELOG.md.tmp.*"))


def test_render_rejects_v_prefixed_version_file(repo, capsys):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "1.0.0.md").write_text("## 1.0.0\n\n- a\n")
    (d / "v0.9.0.md").write_text("## v0.9.0\n\n- b\n")
    rc = changelog.render_main([])
    assert rc == 1
    assert "v0.9.0.md" in capsys.readouterr().err


def test_render_readme_and_legacy_not_versions(repo):
    d = repo / "CHANGELOG"
    d.mkdir()
    (d / "README.md").write_text("README placeholder content\n")
    (d / "1.0.0.md").write_text("## 1.0.0\n\n- a\n")
    rc = changelog.render_main([])
    assert rc == 0
    assert "README placeholder content" not in (repo / "CHANGELOG.md").read_text()


# --- orchestrator -----------------------------------------------------------


def test_orchestrator_no_args_usage(repo, capsys):
    rc = changelog.orchestrator_main([])
    assert rc == 2
    assert "usage:" in capsys.readouterr().err


def test_orchestrator_help_exit0(repo, capsys):
    rc = changelog.orchestrator_main(["--help"])
    assert rc == 0
    assert "usage:" in capsys.readouterr().out


def test_orchestrator_unknown_command(repo, capsys):
    rc = changelog.orchestrator_main(["frobnicate"])
    assert rc == 2
    assert "unknown command" in capsys.readouterr().err


def test_orchestrator_new_version_cuts_and_renders(repo):
    changelog.add_main(["1", "- bullet 1"])
    changelog.add_main(["2", "- bullet 2"])
    rc = changelog.orchestrator_main(["new-version", "0.1.0"])
    assert rc == 0
    assert (repo / "CHANGELOG" / "0.1.0.md").exists()
    assert not (repo / "CHANGELOG" / "unreleased-pr-1.md").exists()
    text = (repo / "CHANGELOG.md").read_text()
    assert text.count("## 0.1.0") == 1


def test_orchestrator_new_version_without_arg(repo, capsys):
    rc = changelog.orchestrator_main(["new-version"])
    assert rc == 2
    assert "usage:" in capsys.readouterr().err
