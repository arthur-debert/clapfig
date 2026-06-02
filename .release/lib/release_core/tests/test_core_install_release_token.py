"""install_release_token verb — token validation + discovery + set-then-verify.

Offline: the gh boundary (rest / secret_set / secret_list) is monkeypatched and
the curl /user call is exercised through its pure parser/validator (no network).
NEVER sets a real secret. Asserts the required-scope contract, the
set-then-relist verification, and the summary lines.
"""

from __future__ import annotations

from release_core import gh
from release_core.verbs import install_release_token as irt

# --------------------------------------------------------------------------
# _parse_curl_response — the sed/awk/grep extraction
# --------------------------------------------------------------------------


def test_parse_curl_response_splits_code_scopes_body():
    raw = (
        "HTTP/2 200\r\n"
        "x-oauth-scopes: repo, read:org\r\n"
        "content-type: application/json\r\n"
        "\r\n"
        '{"login":"octocat"}\n'
        "HTTP_CODE:200\n"
    )
    code, body, scopes = irt._parse_curl_response(raw)
    assert code == "200"
    assert scopes == "repo, read:org"
    assert '"login":"octocat"' in body


def test_parse_curl_response_no_scopes_header():
    raw = 'HTTP/2 200\r\n\r\n{"login":"x"}\nHTTP_CODE:200\n'
    _, _, scopes = irt._parse_curl_response(raw)
    assert scopes == ""


# --------------------------------------------------------------------------
# validate_token — precedence
# --------------------------------------------------------------------------


def test_validate_ok_with_required_scopes():
    ok, err, info = irt.validate_token("200", '{"login":"octocat"}', "repo, read:org")
    assert ok is True
    assert err == ""
    assert info == ["token authenticates as: octocat", "token scopes: repo, read:org"]


def test_validate_bad_http_code():
    ok, err, info = irt.validate_token("401", "", "repo")
    assert ok is False
    assert "authentication failed (http 401)" in err


def test_validate_200_but_no_login():
    ok, err, _ = irt.validate_token("200", "{}", "repo, read:org")
    assert ok is False
    assert "authentication failed (http 200)" in err


def test_validate_no_scopes_is_fine_grained_pat():
    ok, err, _ = irt.validate_token("200", '{"login":"x"}', "")
    assert ok is False
    assert "fine-grained PAT" in err
    assert "repo read:org" in err


def test_validate_missing_one_scope():
    ok, err, _ = irt.validate_token("200", '{"login":"x"}', "repo")
    assert ok is False
    assert "missing required OAuth scope(s): read:org" in err
    assert "Token has: repo" in err


def test_validate_scopes_extra_ones_ok():
    ok, _, _ = irt.validate_token("200", '{"login":"x"}', "repo, read:org, gist, workflow")
    assert ok is True


# --------------------------------------------------------------------------
# discovery
# --------------------------------------------------------------------------


def test_discover_keeps_only_ruled(monkeypatch):
    monkeypatch.setattr(irt, "_list_repos", lambda owner: ["o/a", "o/b"])
    monkeypatch.setattr(irt, "_has_ruleset", lambda r: r == "o/a")
    assert irt.discover_onboarded_repos(["o"]) == ["o/a"]


def test_has_ruleset_tolerates_gh_error(monkeypatch):
    def boom(path):
        raise gh.GhError("403")

    monkeypatch.setattr(gh, "rest", boom)
    assert irt._has_ruleset("o/x") is False


# --------------------------------------------------------------------------
# main — stdin/validation/set-verify loop
# --------------------------------------------------------------------------


class _FakeStdin:
    def __init__(self, text, tty=False):
        self._text = text
        self._tty = tty

    def isatty(self):
        return self._tty

    def read(self):
        return self._text


def _patch_validation(monkeypatch, *, ok=True):
    monkeypatch.setattr(
        irt, "_curl_user", lambda token: ("200", '{"login":"octocat"}', "repo, read:org")
    )
    if not ok:
        monkeypatch.setattr(irt, "validate_token", lambda *a: (False, "error: bad", []))


def test_main_tty_stdin_exits_64(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("", tty=True))
    rc = irt.main([])
    assert rc == 64
    assert "pipe the PAT to stdin" in capsys.readouterr().err


def test_main_empty_token_exits_64(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("\n  \n"))
    rc = irt.main([])
    assert rc == 64
    assert "empty token" in capsys.readouterr().err


def test_main_invalid_token_exits_1(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_xxx"))
    _patch_validation(monkeypatch, ok=False)
    rc = irt.main([])
    assert rc == 1
    assert "error: bad" in capsys.readouterr().err


def test_main_strips_whitespace_from_token(monkeypatch, capsys):
    seen = {}
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_ab cd\n\r"))

    def cap(token):
        seen["token"] = token
        return ("200", '{"login":"o"}', "repo, read:org")

    monkeypatch.setattr(irt, "_curl_user", cap)
    monkeypatch.setattr(irt, "discover_onboarded_repos", lambda o: [])
    irt.main([])
    assert seen["token"] == "ghp_abcd"


def test_main_no_repos_exits_1(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_xxx"))
    _patch_validation(monkeypatch)
    monkeypatch.setattr(irt, "discover_onboarded_repos", lambda o: [])
    rc = irt.main([])
    assert rc == 1
    assert "no onboarded repos found" in capsys.readouterr().err


def test_main_set_then_verify_ok(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_xxx"))
    _patch_validation(monkeypatch)
    monkeypatch.setattr(irt, "discover_onboarded_repos", lambda o: ["o/a", "o/b"])
    set_calls = []
    monkeypatch.setattr(gh, "secret_set", lambda n, v, *, repo: set_calls.append((repo, n)))
    monkeypatch.setattr(gh, "secret_list", lambda repo: ["RELEASE_TOKEN", "OTHER"])
    rc = irt.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert set_calls == [("o/a", "RELEASE_TOKEN"), ("o/b", "RELEASE_TOKEN")]
    assert "  ok   o/a" in out
    assert "2 repos, 2 verified set, 0 failure(s)" in out


def test_main_set_succeeds_but_relist_missing(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_xxx"))
    _patch_validation(monkeypatch)
    monkeypatch.setattr(irt, "discover_onboarded_repos", lambda o: ["o/a"])
    monkeypatch.setattr(gh, "secret_set", lambda n, v, *, repo: None)
    monkeypatch.setattr(gh, "secret_list", lambda repo: ["SOMETHING_ELSE"])
    rc = irt.main([])
    cap = capsys.readouterr()
    assert rc == 1
    assert "absent on re-list" in cap.err
    assert "didn't persist" in cap.err
    assert "1 repos, 0 verified set, 1 failure(s)" in cap.out


def test_main_set_raises_counts_failure(monkeypatch, capsys):
    monkeypatch.setattr(irt.sys, "stdin", _FakeStdin("ghp_xxx"))
    _patch_validation(monkeypatch)
    monkeypatch.setattr(irt, "discover_onboarded_repos", lambda o: ["o/a"])

    def boom(n, v, *, repo):
        raise gh.GhError("nope")

    monkeypatch.setattr(gh, "secret_set", boom)
    rc = irt.main([])
    cap = capsys.readouterr()
    assert rc == 1
    assert "FAIL o/a — gh secret set" in cap.err


def test_main_help_exits_0(capsys):
    rc = irt.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out
    assert "install-release-token" in out


def test_main_unknown_flag_exits_64(capsys):
    rc = irt.main(["--bogus"])
    assert rc == 64
