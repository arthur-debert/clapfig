"""install_release_secrets verb — secret-sourcing + discovery + set-loop.

Offline: the gh boundary (rest / secret_set) and the local `gh repo list`
porcelain are monkeypatched; the Apple-auth sources are synthesized in a tmp
auth dir. NEVER sets a real secret. Asserts the set-of-7 contract, the optional
NPM 8th slot, the source-precedence errors, and the dry-run/summary lines.
"""

from __future__ import annotations

import base64

import pytest
from release_core import gh
from release_core.verbs import install_release_secrets as irs

# --------------------------------------------------------------------------
# fixtures
# --------------------------------------------------------------------------


@pytest.fixture
def auth_dir(tmp_path):
    """A complete Apple-auth dir: p12 + password + AuthKey_*.p8 + issuer."""
    d = tmp_path / "auth"
    d.mkdir()
    (d / "developerID_application.p12").write_bytes(b"\x00p12-bytes\x01")
    (d / "p12_password.txt").write_text("hunter2\n")
    (d / "AuthKey_ABC123XYZ.p8").write_bytes(b"\x00p8-bytes\x01")
    (d / "asc_issuer_id.txt").write_text("  issuer-uuid  \n")
    return str(d)


def _full_env():
    return {"CRATES_IO_KEY": "crates-tok", "HOMEBREW_TAP_TOKEN": "brew-tok"}


# --------------------------------------------------------------------------
# collect_secrets — the secret-source contract
# --------------------------------------------------------------------------


def test_collect_secrets_seven_slots_in_order(auth_dir):
    secrets, npm = irs.collect_secrets(auth_dir, _full_env())
    names = [n for n, _ in secrets]
    assert names == [
        "APPLE_CERTIFICATE_P12_BASE64",
        "APPLE_CERTIFICATE_PASSWORD",
        "ASC_API_KEY_BASE64",
        "ASC_API_KEY_ID",
        "ASC_API_ISSUER_ID",
        "CRATES_IO_KEY",
        "HOMEBREW_TAP_TOKEN",
    ]
    assert npm is False


def test_collect_secrets_values_sourced_correctly(auth_dir):
    secrets, _ = irs.collect_secrets(auth_dir, _full_env())
    by = dict(secrets)
    assert by["APPLE_CERTIFICATE_P12_BASE64"] == base64.b64encode(b"\x00p12-bytes\x01").decode()
    assert by["ASC_API_KEY_BASE64"] == base64.b64encode(b"\x00p8-bytes\x01").decode()
    # ASC_API_KEY_ID parsed from the AuthKey_<id>.p8 filename
    assert by["ASC_API_KEY_ID"] == "ABC123XYZ"
    # issuer stripped of newlines + spaces (tr -d '\n\r ')
    assert by["ASC_API_ISSUER_ID"] == "issuer-uuid"
    # password kept verbatim (NOT stripped — matches `cat`)
    assert by["APPLE_CERTIFICATE_PASSWORD"] == "hunter2\n"
    assert by["CRATES_IO_KEY"] == "crates-tok"
    assert by["HOMEBREW_TAP_TOKEN"] == "brew-tok"


def test_collect_secrets_npm_adds_eighth_slot(auth_dir):
    env = {**_full_env(), "NPM_TOKEN": "npm-tok"}
    secrets, npm = irs.collect_secrets(auth_dir, env)
    assert npm is True
    assert secrets[-1] == ("NPM_TOKEN", "npm-tok")
    assert len(secrets) == 8


def test_collect_secrets_empty_npm_is_skipped(auth_dir):
    env = {**_full_env(), "NPM_TOKEN": ""}
    secrets, npm = irs.collect_secrets(auth_dir, env)
    assert npm is False
    assert len(secrets) == 7


def test_missing_p12_raises(auth_dir, tmp_path):
    import os

    os.remove(os.path.join(auth_dir, "developerID_application.p12"))
    with pytest.raises(irs.SourceError, match="developerID_application.p12"):
        irs.collect_secrets(auth_dir, _full_env())


def test_missing_password_file_has_repack_instructions(auth_dir):
    import os

    os.remove(os.path.join(auth_dir, "p12_password.txt"))
    with pytest.raises(irs.SourceError, match="re-pack the cert"):
        irs.collect_secrets(auth_dir, _full_env())


def test_missing_p8_raises(auth_dir):
    import os

    os.remove(os.path.join(auth_dir, "AuthKey_ABC123XYZ.p8"))
    with pytest.raises(irs.SourceError, match=r"AuthKey_\*\.p8"):
        irs.collect_secrets(auth_dir, _full_env())


def test_missing_issuer_raises(auth_dir):
    import os

    os.remove(os.path.join(auth_dir, "asc_issuer_id.txt"))
    with pytest.raises(irs.SourceError, match="asc_issuer_id.txt"):
        irs.collect_secrets(auth_dir, _full_env())


def test_missing_crates_env_raises(auth_dir):
    with pytest.raises(irs.SourceError, match="CRATES_IO_KEY"):
        irs.collect_secrets(auth_dir, {"HOMEBREW_TAP_TOKEN": "x"})


def test_missing_homebrew_env_raises(auth_dir):
    with pytest.raises(irs.SourceError, match="HOMEBREW_TAP_TOKEN"):
        irs.collect_secrets(auth_dir, {"CRATES_IO_KEY": "x"})


# --------------------------------------------------------------------------
# discovery
# --------------------------------------------------------------------------


def test_discover_filters_by_ruleset_and_cargo(monkeypatch):
    monkeypatch.setattr(irs, "_list_repos", lambda owner: ["o/a", "o/b", "o/c"])
    # a: ruleset + cargo (keep); b: no ruleset (drop); c: ruleset, no cargo (drop)
    ruled = {"o/a", "o/c"}
    carg = {"o/a"}
    monkeypatch.setattr(irs, "_has_ruleset", lambda r: r in ruled)
    monkeypatch.setattr(irs, "_has_cargo_toml", lambda r: r in carg)
    assert irs.discover_rust_repos(["o"]) == ["o/a"]


def test_has_ruleset_tolerates_gh_error(monkeypatch):
    def boom(path):
        raise gh.GhError("403")

    monkeypatch.setattr(gh, "rest", boom)
    assert irs._has_ruleset("o/x") is False


# --------------------------------------------------------------------------
# main — set loop, dry-run, summary
# --------------------------------------------------------------------------


def _patch_full(monkeypatch, auth_dir, set_calls, *, fail_names=()):
    monkeypatch.setenv("CRATES_IO_KEY", "crates-tok")
    monkeypatch.setenv("HOMEBREW_TAP_TOKEN", "brew-tok")
    monkeypatch.delenv("NPM_TOKEN", raising=False)

    def fake_set(name, value, *, repo):
        if name in fail_names:
            raise gh.GhError("boom")
        set_calls.append((repo, name))

    monkeypatch.setattr(gh, "secret_set", fake_set)


def test_main_explicit_repos_sets_seven(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls)
    rc = irs.main(["--auth-dir", auth_dir, "--repos", "o/a,o/b"])
    out = capsys.readouterr().out
    assert rc == 0
    # 7 secrets * 2 repos
    assert len(calls) == 14
    assert {r for r, _ in calls} == {"o/a", "o/b"}
    assert "all 7 secrets set" in out


def test_main_dry_run_sets_nothing(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls)
    rc = irs.main(["--auth-dir", auth_dir, "--repos", "o/a", "--dry-run"])
    out = capsys.readouterr().out
    assert rc == 0
    assert calls == []  # never touched a real secret
    assert "[dry] APPLE_CERTIFICATE_P12_BASE64" in out


def test_main_npm_present_reports_eight(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls)
    monkeypatch.setenv("NPM_TOKEN", "npm-tok")
    rc = irs.main(["--auth-dir", auth_dir, "--repos", "o/a"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "all 8 secrets set" in out
    assert ("o/a", "NPM_TOKEN") in calls


def test_main_npm_absent_prints_skip_notice(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls)
    irs.main(["--auth-dir", auth_dir, "--repos", "o/a"])
    out = capsys.readouterr().out
    assert "NPM_TOKEN not in env" in out


def test_main_failure_returns_1_and_reports(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls, fail_names={"CRATES_IO_KEY"})
    rc = irs.main(["--auth-dir", auth_dir, "--repos", "o/a"])
    err = capsys.readouterr()
    assert rc == 1
    assert "FAIL CRATES_IO_KEY" in err.out
    assert "with failures" in err.err


def test_main_no_repos_found_returns_1(monkeypatch, auth_dir, capsys):
    calls: list = []
    _patch_full(monkeypatch, auth_dir, calls)
    monkeypatch.setattr(irs, "discover_rust_repos", lambda owners: [])
    rc = irs.main(["--auth-dir", auth_dir])
    assert rc == 1
    assert "no rust repos found" in capsys.readouterr().err


def test_main_missing_source_returns_1_before_discovery(monkeypatch, auth_dir, capsys):
    monkeypatch.delenv("CRATES_IO_KEY", raising=False)
    monkeypatch.setenv("HOMEBREW_TAP_TOKEN", "x")
    called = {"discover": False}
    monkeypatch.setattr(
        irs,
        "discover_rust_repos",
        lambda owners: called.__setitem__("discover", True) or [],
    )
    rc = irs.main(["--auth-dir", auth_dir, "--repos", "o/a"])
    assert rc == 1
    assert "CRATES_IO_KEY" in capsys.readouterr().err
    assert called["discover"] is False


def test_main_help_exits_0(capsys):
    rc = irs.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out
    assert "install-release-secrets" in out


def test_main_unknown_flag_exits_64(capsys):
    rc = irs.main(["--bogus"])
    assert rc == 64
