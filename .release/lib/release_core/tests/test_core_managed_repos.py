"""managed_repos verb — manifest parsing, path join, filter, mode dispatch.

Fully offline: a fixture manifest via MANAGED_REPOS_MANIFEST + a synthetic
REPOS_ROOT. No gh, no network (clone mode's gh boundary is not exercised here —
it is covered by the BATS contract test against a stub gh).
"""

from __future__ import annotations

import os

import pytest
from release_core.verbs import managed_repos

FIXTURE = """\
projects:
  lex:
    - { repo: lex-fmt/lex,   path: lex-fmt/lex }
    - { repo: lex-fmt/comms, path: lex-fmt/comms }
  phos:
    - { repo: arthur-debert/phos-app, path: phos/phos-app }
  dodot:
    - { repo: arthur-debert/dodot, path: dodot }
"""


@pytest.fixture
def fleet(tmp_path, monkeypatch):
    manifest = tmp_path / "manifest.yaml"
    manifest.write_text(FIXTURE)
    root = tmp_path / "root"
    # lex-fmt/lex and dodot "exist" (.git); comms and phos-app do not.
    (root / "lex-fmt" / "lex" / ".git").mkdir(parents=True)
    (root / "dodot" / ".git").mkdir(parents=True)
    monkeypatch.setenv("MANAGED_REPOS_MANIFEST", str(manifest))
    monkeypatch.setenv("REPOS_ROOT", str(root))
    monkeypatch.delenv("MANAGED_REPOS_SCRIPT_DIR", raising=False)
    return root


def test_list_prints_every_repo_in_manifest_order(fleet, capsys):
    rc = managed_repos.main(["--list"])
    assert rc == 0
    out = capsys.readouterr().out.splitlines()
    assert out == ["lex-fmt/lex", "lex-fmt/comms", "arthur-debert/phos-app", "arthur-debert/dodot"]


def test_list_is_the_default_mode(fleet, capsys):
    managed_repos.main([])
    assert capsys.readouterr().out.splitlines()[0] == "lex-fmt/lex"


def test_paths_joins_root_and_marks_found_missing(fleet, capsys):
    rc = managed_repos.main(["--paths"])
    assert rc == 0
    lines = capsys.readouterr().out.splitlines()
    root = os.environ["REPOS_ROOT"]
    assert lines[0] == f"lex-fmt/lex\t{root}/lex-fmt/lex\tfound"
    assert f"lex-fmt/comms\t{root}/lex-fmt/comms\tmissing" in lines
    assert f"arthur-debert/dodot\t{root}/dodot\tfound" in lines


def test_paths_uses_tabs(fleet, capsys):
    managed_repos.main(["--paths"])
    assert "\t" in capsys.readouterr().out.splitlines()[0]


def test_filter_restricts_and_preserves_manifest_order(fleet, capsys):
    managed_repos.main(["--list", "arthur-debert/dodot", "lex-fmt/lex"])
    # manifest order, not arg order: lex precedes dodot in the manifest.
    assert capsys.readouterr().out.splitlines() == ["lex-fmt/lex", "arthur-debert/dodot"]


def test_filter_with_no_matches_prints_nothing(fleet, capsys):
    rc = managed_repos.main(["--list", "lex-fmt/nonesuch"])
    assert rc == 0
    assert capsys.readouterr().out == ""


def test_unknown_flag_is_usage_error(fleet, capsys):
    rc = managed_repos.main(["--bogus"])
    assert rc == 64
    assert "unknown arg" in capsys.readouterr().err


def test_help_exits_zero(fleet, capsys):
    rc = managed_repos.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out


def test_missing_manifest_exits_2(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("MANAGED_REPOS_MANIFEST", str(tmp_path / "nope.yaml"))
    monkeypatch.delenv("MANAGED_REPOS_SCRIPT_DIR", raising=False)
    rc = managed_repos.main(["--list"])
    assert rc == 2
    assert "manifest not found" in capsys.readouterr().err
