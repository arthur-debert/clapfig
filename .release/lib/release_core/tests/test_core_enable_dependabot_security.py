"""enable_dependabot_security verb — ruleset-membership filter + PUT routing.

The onboarded-repo discovery and the two enable PUTs are exercised against
recorded JSON via monkeypatched gh.rest / proc.run (mock at the data layer —
NEVER touch a live repo's security settings).
"""

from __future__ import annotations

from release_core import gh, proc
from release_core.verbs import enable_dependabot_security as eds


def test_has_ruleset_true_on_match():
    assert eds.has_ruleset([{"id": 1, "name": "main-branch-protection"}]) is True
    assert eds.has_ruleset([{"id": 1, "name": "other"}]) is False
    assert eds.has_ruleset([]) is False
    assert eds.has_ruleset(None) is False


def test_discover_repos_keeps_only_onboarded(monkeypatch):
    listings = {"o1": ["o1/a", "o1/b"], "o2": ["o2/c"]}
    onboarded = {"o1/a", "o2/c"}

    def fake_list(owner):
        return listings.get(owner, [])

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        repo = path[len("repos/") : -len("/rulesets")]
        if repo in onboarded:
            return [{"id": 1, "name": "main-branch-protection"}]
        return []

    monkeypatch.setattr(eds, "_list_owner_repos", fake_list)
    monkeypatch.setattr(gh, "rest", fake_rest)
    assert eds.discover_repos(["o1", "o2"]) == ["o1/a", "o2/c"]


def test_is_onboarded_handles_gherror_as_false(monkeypatch):
    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        raise gh.GhError("404")

    monkeypatch.setattr(gh, "rest", fake_rest)
    assert eds._is_onboarded("o/r") is False


def test_main_explicit_repos_dry_run(monkeypatch, capsys):
    rc = eds.main(["--repos", "o/a,o/b", "--dry-run"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "found 2 repo(s):" in out
    assert "  o/a" in out and "  o/b" in out
    assert out.count("[dry] PUT vulnerability-alerts") == 2
    assert out.count("[dry] PUT automated-security-fixes") == 2


def test_main_explicit_repos_enable_success(monkeypatch, capsys):
    puts = []

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        puts.append((path, method))
        return None

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = eds.main(["--repos", "o/a"])
    out = capsys.readouterr().out
    assert rc == 0
    assert ("repos/o/a/vulnerability-alerts", "PUT") in puts
    assert ("repos/o/a/automated-security-fixes", "PUT") in puts
    assert "    ok   alerts" in out
    assert "    ok   automated-fixes" in out
    assert out.rstrip().endswith("summary: 1 repos, alerts + automated fixes enabled")


def test_main_reports_failures_and_exits_1(monkeypatch, capsys):
    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if "automated-security-fixes" in path:
            raise gh.GhError("500")
        return None

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = eds.main(["--repos", "o/a"])
    captured = capsys.readouterr()
    assert rc == 1
    assert "    ok   alerts" in captured.out
    assert "    FAIL automated-fixes" in captured.out
    assert "1 with failures" in captured.err


def test_main_no_repos_found_exits_1(monkeypatch, capsys):
    monkeypatch.setattr(eds, "discover_repos", lambda owners: [])
    rc = eds.main([])
    assert rc == 1
    assert "no onboarded repos found" in capsys.readouterr().err


def test_main_help(capsys):
    rc = eds.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out
    assert "enable-dependabot-security" in out


def test_main_unknown_flag(capsys):
    assert eds.main(["--nope"]) == 64


def test_list_owner_repos_parses_namewithowner(monkeypatch):
    def fake_run(cmd, **kw):
        import types

        return types.SimpleNamespace(
            returncode=0, stdout='[{"nameWithOwner":"o/a"},{"nameWithOwner":"o/b"}]', stderr=""
        )

    monkeypatch.setattr(proc, "run", fake_run)
    assert eds._list_owner_repos("o") == ["o/a", "o/b"]
