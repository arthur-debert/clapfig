"""release_cut verb — per-Kind version readers, semver bump, literal-version
guard, and the gh-dispatch path.

Offline: the per-Kind readers run against real fixture files in tmp_path; the
git/gh boundary is stubbed at proc.run/proc.out (a recorded command map), never
at subprocess. Mirrors the byte-for-byte contract pinned by
tests/release-cut/release-cut.bats.
"""

from __future__ import annotations

import pytest
from release_core import proc
from release_core.verbs import release_cut

# --- per-Kind version readers (pure file parsing) ---------------------


def test_read_toml_package_version(tmp_path):
    (tmp_path / "Cargo.toml").write_text('[package]\nname = "foo"\nversion = "1.4.2"\n')
    assert release_cut._read_toml_version(str(tmp_path / "Cargo.toml")) == "1.4.2"


def test_read_toml_workspace_package_version(tmp_path):
    (tmp_path / "Cargo.toml").write_text(
        '[workspace.package]\nversion = "0.7.2-rc.2"\nedition = "2024"\n'
    )
    assert release_cut._read_toml_version(str(tmp_path / "Cargo.toml")) == "0.7.2-rc.2"


def test_read_toml_ignores_dependency_section_version(tmp_path):
    (tmp_path / "Cargo.toml").write_text(
        '[workspace.dependencies.serde]\nversion = "1.0.219"\nfeatures = []\n'
    )
    assert release_cut._read_toml_version(str(tmp_path / "Cargo.toml")) is None


def test_read_toml_extension_toplevel(tmp_path):
    # extension.toml shape: top-level version before any [section].
    (tmp_path / "extension.toml").write_text('id = "lex"\nversion = "0.1.2-rc.1"\n')
    assert release_cut._read_toml_version(str(tmp_path / "extension.toml")) == "0.1.2-rc.1"


def test_read_json_version(tmp_path):
    (tmp_path / "package.json").write_text('{\n  "name": "lexed",\n  "version": "0.10.7-rc.2"\n}\n')
    assert release_cut._read_json_version(str(tmp_path / "package.json")) == "0.10.7-rc.2"


def test_read_json_ignores_nested_version(tmp_path):
    (tmp_path / "package.json").write_text(
        '{\n  "name": "foo",\n  "version": "2.0.0",\n'
        '  "devDependencies": { "some-package-version": "9.9.9" }\n}\n'
    )
    assert release_cut._read_json_version(str(tmp_path / "package.json")) == "2.0.0"


def test_read_rust_workspace_only_probes_members(tmp_path, monkeypatch):
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nresolver = "2"\nmembers = ["crates/lib", "crates/cli"]\n'
    )
    (tmp_path / "crates" / "lib").mkdir(parents=True)
    (tmp_path / "crates" / "lib" / "Cargo.toml").write_text(
        '[package]\nname = "foo-lib"\nversion = "5.0.1-rc.1"\n'
    )
    (tmp_path / "crates" / "cli").mkdir(parents=True)
    (tmp_path / "crates" / "cli" / "Cargo.toml").write_text(
        '[package]\nname = "foo"\nversion.workspace = true\n'
    )
    monkeypatch.chdir(tmp_path)
    assert release_cut._read_rust_version() == "5.0.1-rc.1"


def test_read_rust_multiline_members(tmp_path, monkeypatch):
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = [\n    "crates/lib",\n    "crates/cli",\n]\n'
    )
    (tmp_path / "crates" / "lib").mkdir(parents=True)
    (tmp_path / "crates" / "lib" / "Cargo.toml").write_text(
        '[package]\nname = "foo-lib"\nversion = "2.0.0"\n'
    )
    monkeypatch.chdir(tmp_path)
    assert release_cut._read_rust_version() == "2.0.0"


# --- literal-version guard --------------------------------------------


@pytest.mark.parametrize(
    "good",
    [
        "1.2.3",
        "0.0.1",
        "1.0.0-rc.1",
        "10.20.30-beta.2",
        # A 0 in a numeric field is fine (NAT = '0|[1-9][0-9]*'); only a
        # LEADING zero on a multi-digit field is rejected.
        "0.0.0",
        "1.0.0-0",  # bare '0' prerelease identifier is NAT-valid
    ],
)
def test_literal_version_accepts(good):
    assert release_cut._is_valid_literal_version(good)


@pytest.mark.parametrize(
    "bad",
    [
        "v1.2.3",
        "V1.2.3",
        "not-a-version",
        "1.2",
        "1.2.3+build.7",
        "minor",
        # Leading-zero parity (FU2): the bash semver-tool's NAT field
        # ('0|[1-9][0-9]*') REJECTS these; release_core.version.parse would
        # silently ACCEPT + normalize them, which was the reject-path break.
        "01.0.0",  # leading-zero major
        "1.01.0",  # leading-zero minor
        "1.0.01",  # leading-zero patch
        "1.00.0",  # multi-zero minor
        "1.0.0-01",  # leading-zero numeric prerelease identifier
        "01.0.0+build",  # leading zero is still caught even with a build part
    ],
)
def test_literal_version_rejects(bad):
    assert not release_cut._is_valid_literal_version(bad)


def test_leading_zero_literal_exits_2(gh_dispatch, capsys):
    """End-to-end: `release-cut 01.0.0` must hit the invalid-literal branch
    (exit 2, same message), not be silently accepted + normalized to 1.0.0."""
    rc = release_cut.main(["01.0.0"])
    assert rc == 2
    err = capsys.readouterr().err
    assert "version must be" in err
    # And it must NOT have dispatched a (normalized) gh workflow run.
    assert not any(c[:3] == ["gh", "workflow", "run"] for c in gh_dispatch)


# --- main() dispatch (git/gh stubbed at the data layer) ---------------


@pytest.fixture
def gh_dispatch(monkeypatch, tmp_path):
    """A repo dir with release.yml + a recording of the gh dispatch call."""
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    monkeypatch.chdir(tmp_path)
    calls: list[list[str]] = []

    def fake_out(cmd, **kw):
        if cmd[:3] == ["git", "rev-parse", "--show-toplevel"]:
            return str(tmp_path)
        raise AssertionError(f"unexpected proc.out: {cmd}")

    def fake_run(cmd, **kw):
        calls.append(cmd)
        return _completed(cmd, returncode=0, stdout="")

    monkeypatch.setattr(proc, "out", fake_out)
    monkeypatch.setattr(proc, "run", fake_run)
    monkeypatch.setattr(release_cut.shutil, "which", lambda _name: "/usr/bin/gh")
    return calls


def _completed(cmd, *, returncode=0, stdout="", stderr=""):
    import subprocess

    return subprocess.CompletedProcess(cmd, returncode, stdout, stderr)


def test_literal_version_dispatches(gh_dispatch, capsys):
    rc = release_cut.main(["1.2.3"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "Triggering release.yml for v1.2.3..." in out
    assert "Workflow queued" in out
    assert ["gh", "workflow", "run", "release.yml", "-f", "version=1.2.3"] in gh_dispatch


def test_bad_version_exits_2(gh_dispatch, capsys):
    rc = release_cut.main(["not-a-version"])
    assert rc == 2
    assert "version must be" in capsys.readouterr().err


def test_no_args_exits_2(capsys):
    rc = release_cut.main([])
    assert rc == 2
    assert "usage:" in capsys.readouterr().err


def test_help_exits_0(capsys):
    rc = release_cut.main(["--help"])
    assert rc == 0
    assert "usage:" in capsys.readouterr().err


def test_no_release_yml_is_graceful_noop(monkeypatch, tmp_path, capsys):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    rc = release_cut.main(["minor"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "no .github/workflows/release.yml" in out
    assert "nothing to do" in out


def test_bump_reads_kind_and_dispatches(monkeypatch, tmp_path, capsys):
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    (tmp_path / "Cargo.toml").write_text('[package]\nname = "foo"\nversion = "1.4.2"\n')
    monkeypatch.chdir(tmp_path)
    calls: list[list[str]] = []

    def fake_run(cmd, **kw):
        calls.append(cmd)
        return _completed(cmd, returncode=0, stdout="")

    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    monkeypatch.setattr(proc, "run", fake_run)
    monkeypatch.setattr(release_cut.shutil, "which", lambda _name: "/usr/bin/gh")

    rc = release_cut.main(["minor"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "Bumping minor: 1.4.2 -> 1.5.0" in out
    assert ["gh", "workflow", "run", "release.yml", "-f", "version=1.5.0"] in calls


def test_bump_on_unclassifiable_dir_errors(monkeypatch, tmp_path, capsys):
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    (tmp_path / "README.md").write_text("nothing detectable\n")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    rc = release_cut.main(["minor"])
    assert rc == 1
    assert "detect-kind could not identify" in capsys.readouterr().err


def test_gh_missing_exits_1(monkeypatch, tmp_path, capsys):
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    monkeypatch.setattr(release_cut.shutil, "which", lambda _name: None)
    rc = release_cut.main(["1.2.3"])
    assert rc == 1
    assert "gh CLI not found" in capsys.readouterr().err


def test_workspace_only_root_git_tag_kind(monkeypatch, tmp_path, capsys):
    # go-cli reads `git describe`; stub it returning a tag.
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    (tmp_path / "go.mod").write_text("module example.com/foo\n")
    monkeypatch.chdir(tmp_path)
    calls: list[list[str]] = []

    def fake_run(cmd, **kw):
        calls.append(cmd)
        if cmd[:2] == ["git", "describe"]:
            return _completed(cmd, returncode=0, stdout="v0.3.1\n")
        return _completed(cmd, returncode=0, stdout="")

    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    monkeypatch.setattr(proc, "run", fake_run)
    monkeypatch.setattr(release_cut.shutil, "which", lambda _name: "/usr/bin/gh")

    rc = release_cut.main(["minor"])
    assert rc == 0
    assert "Bumping minor: 0.3.1 -> 0.4.0" in capsys.readouterr().out


def test_go_cli_no_tags_errors(monkeypatch, tmp_path, capsys):
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "release.yml").write_text("name: release\n")
    (tmp_path / "go.mod").write_text("module example.com/foo\n")
    monkeypatch.chdir(tmp_path)

    def fake_run(cmd, **kw):
        if cmd[:2] == ["git", "describe"]:
            return _completed(cmd, returncode=128, stdout="", stderr="no tags")
        return _completed(cmd, returncode=0, stdout="")

    monkeypatch.setattr(proc, "out", lambda cmd, **kw: str(tmp_path))
    monkeypatch.setattr(proc, "run", fake_run)
    rc = release_cut.main(["minor"])
    assert rc == 1
    err = capsys.readouterr().err
    assert "no git tags found" in err
    assert "explicit version" in err
