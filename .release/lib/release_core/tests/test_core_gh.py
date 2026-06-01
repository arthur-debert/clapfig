"""gh — the GitHub/git chokepoint. Mocked at the proc boundary; no network.

These tests cover the additive helpers landed for the remaining Bucket-A verbs
(`secret_set`/`secret_list` for install-release-{secrets,token}; the `body=`
arm of `rest()` for apply-ruleset's nested-payload PUT/POST). The existing
rest/graphql/git/issue_list surface is exercised elsewhere; here we assert the
exact `gh` argv each helper builds and how it parses the reply.
"""

from __future__ import annotations

import json
import subprocess

import pytest
from release_core import gh
from release_core.gh import GhError


class _Recorder:
    """Stands in for proc.run: records the argv + stdin, replays a canned reply."""

    def __init__(self, stdout="", returncode=0, stderr=""):
        self.stdout = stdout
        self.returncode = returncode
        self.stderr = stderr
        self.calls = []

    def __call__(self, cmd, *, input=None, check=False):  # noqa: A002 — mirrors proc.run
        self.calls.append((cmd, input))
        return subprocess.CompletedProcess(
            cmd, self.returncode, stdout=self.stdout, stderr=self.stderr
        )


@pytest.fixture
def gh_on_path(monkeypatch):
    """Pretend `gh` is installed so _gh doesn't short-circuit."""
    monkeypatch.setattr(gh.shutil, "which", lambda _: "/usr/bin/gh")


# ─── secret_set ──────────────────────────────────────────────────────────────


def test_secret_set_builds_argv_and_pipes_value(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    gh.secret_set("CRATES_IO_KEY", "s3cr3t", repo="arthur-debert/dodot")
    cmd, stdin = rec.calls[0]
    assert cmd == ["gh", "secret", "set", "CRATES_IO_KEY", "-R", "arthur-debert/dodot"]
    assert stdin == "s3cr3t"


def test_secret_set_raises_on_failure(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder(returncode=1, stderr="HTTP 403"))
    with pytest.raises(GhError):
        gh.secret_set("X", "v", repo="o/r")


# ─── secret_list ─────────────────────────────────────────────────────────────


def test_secret_list_returns_names_only(gh_on_path, monkeypatch):
    out = "RELEASE_TOKEN\nCRATES_IO_KEY\n"
    monkeypatch.setattr(gh.proc, "run", _Recorder(stdout=out))
    assert gh.secret_list("o/r") == ["RELEASE_TOKEN", "CRATES_IO_KEY"]


def test_secret_list_empty(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder(stdout="\n"))
    assert gh.secret_list("o/r") == []


def test_secret_list_builds_argv(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="A\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    gh.secret_list("o/r")
    assert rec.calls[0][0] == [
        "gh",
        "secret",
        "list",
        "-R",
        "o/r",
        "--json",
        "name",
        "-q",
        ".[].name",
    ]


# ─── rest(body=...) ──────────────────────────────────────────────────────────


def test_rest_body_pipes_json_via_input(gh_on_path, monkeypatch):
    rec = _Recorder(stdout='{"id": 99}')
    monkeypatch.setattr(gh.proc, "run", rec)
    payload = {"name": "main-branch-protection", "rules": [{"type": "x"}]}
    result = gh.rest("repos/o/r/rulesets", method="POST", body=payload)
    assert result == {"id": 99}
    cmd, stdin = rec.calls[0]
    assert cmd == ["gh", "api", "-X", "POST", "--input", "-", "repos/o/r/rulesets"]
    assert json.loads(stdin) == payload


def test_rest_body_put_returns_none_on_empty(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder(stdout=""))
    assert gh.rest("repos/o/r/rulesets/5", method="PUT", body={"a": 1}) is None


def test_rest_fields_and_body_are_mutually_exclusive(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder())
    with pytest.raises(GhError):
        gh.rest("x", fields={"a": "b"}, body={"c": "d"})


def test_rest_plain_get_still_works(gh_on_path, monkeypatch):
    rec = _Recorder(stdout='{"default_branch": "main"}')
    monkeypatch.setattr(gh.proc, "run", rec)
    assert gh.rest("repos/o/r") == {"default_branch": "main"}
    cmd, stdin = rec.calls[0]
    assert cmd == ["gh", "api", "repos/o/r"]
    assert stdin is None


# ─── porcelain wrappers (Phase 1 chokepoint consolidation) ───────────────────
#
# Each test asserts the wrapper builds the BYTE-IDENTICAL argv its call site
# used before the sweep (so the offline BATS `gh` stubs keep matching).


def _argv(rec):
    return rec.calls[0][0]


# repo_view --------------------------------------------------------------------


def test_repo_view_json_and_q_strips(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="arthur-debert/dodot\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # apply-ruleset / audit-repo / gh-release-issue call site (-q spelling)
    assert gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner") == "arthur-debert/dodot"
    assert _argv(rec) == ["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"]


def test_repo_view_jq_spelling(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="o/r\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # done-check call site uses --jq, not -q
    gh.repo_view(json_fields=["nameWithOwner"], jq=".nameWithOwner", check=False)
    assert _argv(rec) == ["gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"]


def test_repo_view_bare_repo_arg(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    # audit-smoke-test access-check call site: `gh repo view <repo>`
    res = gh.repo_view(repo="o/r", check=False)
    assert res.returncode == 0
    assert _argv(rec) == ["gh", "repo", "view", "o/r"]


def test_repo_view_check_false_returns_completed_process(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder(returncode=1, stderr="nope"))
    res = gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner", check=False)
    assert res.returncode == 1


def test_repo_view_check_true_raises(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", _Recorder(returncode=1, stderr="nope"))
    with pytest.raises(GhError):
        gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner")


# repo_list --------------------------------------------------------------------


def test_repo_list_with_jq_returns_stripped(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="o/a\no/b\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # install-release-{secrets,token} call site
    out = gh.repo_list("o", limit=200, json_fields=["nameWithOwner"], jq=".[].nameWithOwner")
    assert out == "o/a\no/b"
    assert _argv(rec) == [
        "gh",
        "repo",
        "list",
        "o",
        "--limit",
        "200",
        "--json",
        "nameWithOwner",
        "--jq",
        ".[].nameWithOwner",
    ]


def test_repo_list_check_false_no_jq(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="[]")
    monkeypatch.setattr(gh.proc, "run", rec)
    # enable-dependabot-security call site (no --jq; parses JSON itself)
    res = gh.repo_list("o", limit=200, json_fields=["nameWithOwner"], check=False)
    assert res.returncode == 0
    assert _argv(rec) == ["gh", "repo", "list", "o", "--limit", "200", "--json", "nameWithOwner"]


# repo_clone -------------------------------------------------------------------


def test_repo_clone_argv(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    res = gh.repo_clone("o/r", "/tmp/dest")
    assert res.returncode == 0
    assert _argv(rec) == ["gh", "repo", "clone", "o/r", "/tmp/dest"]


# pr_list ----------------------------------------------------------------------


def test_pr_list_head_json_q(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="42\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # gh-release-issue collect_context call site
    assert gh.pr_list(head="feat/x", json_fields=["number"], q=".[0].number // empty") == "42"
    assert _argv(rec) == [
        "gh",
        "pr",
        "list",
        "--head",
        "feat/x",
        "--json",
        "number",
        "-q",
        ".[0].number // empty",
    ]


# pr_create --------------------------------------------------------------------


def test_pr_create_full_argv(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="https://github.com/o/r/pull/7\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # audit-smoke-test call site (repo+base+head)
    res = gh.pr_create(repo="o/r", base="main", head="b", title="T", body="B")
    assert res.returncode == 0
    assert _argv(rec) == [
        "gh",
        "pr",
        "create",
        "--repo",
        "o/r",
        "--base",
        "main",
        "--head",
        "b",
        "--title",
        "T",
        "--body",
        "B",
    ]


def test_pr_create_repo_only(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="url\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # release-lex call site (no base/head)
    gh.pr_create(repo="o/r", title="T", body="B")
    assert _argv(rec) == ["gh", "pr", "create", "--repo", "o/r", "--title", "T", "--body", "B"]


# pr_merge ---------------------------------------------------------------------


def test_pr_merge_streams_and_argv(gh_on_path, monkeypatch):
    seen = {}

    def fake_run(cmd, *, capture_output=True, check=False):
        seen["cmd"] = cmd
        seen["capture_output"] = capture_output
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(gh.proc, "run", fake_run)
    gh.pr_merge("7", repo="o/r", squash=True, delete_branch=True, admin=True)
    assert seen["cmd"] == [
        "gh",
        "pr",
        "merge",
        "7",
        "--repo",
        "o/r",
        "--squash",
        "--delete-branch",
        "--admin",
    ]
    assert seen["capture_output"] is False


def test_pr_merge_raises_on_failure(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 1))
    with pytest.raises(GhError):
        gh.pr_merge("7", repo="o/r", squash=True)


# pr_comment / pr_close --------------------------------------------------------


def test_pr_comment_argv(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    assert gh.pr_comment("https://x/pull/1", body="hi").returncode == 0
    assert _argv(rec) == ["gh", "pr", "comment", "https://x/pull/1", "--body", "hi"]


def test_pr_close_argv(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    gh.pr_close("7", repo="o/r", delete_branch=True, comment="done")
    assert _argv(rec) == [
        "gh",
        "pr",
        "close",
        "7",
        "--repo",
        "o/r",
        "--delete-branch",
        "--comment",
        "done",
    ]


# run_list ---------------------------------------------------------------------


def test_run_list_workflow_split_form(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="https://x/run\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # gh-release-issue call site: split `--workflow <name>` + branch + url query
    gh.run_list(
        workflow="copilot-review.yml",
        branch="feat/x",
        limit=1,
        json_fields=["url"],
        q=".[0].url // empty",
    )
    assert _argv(rec) == [
        "gh",
        "run",
        "list",
        "--workflow",
        "copilot-review.yml",
        "--branch",
        "feat/x",
        "--limit",
        "1",
        "--json",
        "url",
        "-q",
        ".[0].url // empty",
    ]


def test_run_list_workflow_eq_form(gh_on_path, monkeypatch):
    rec = _Recorder(stdout='[{"databaseId": 5}]')
    monkeypatch.setattr(gh.proc, "run", rec)
    # release-lex call site: joined `--workflow=release.yml` + repo + commit
    gh.run_list(
        repo="o/r",
        workflow_eq="release.yml",
        commit="abc123",
        limit=1,
        json_fields=["databaseId"],
    )
    assert _argv(rec) == [
        "gh",
        "run",
        "list",
        "--repo",
        "o/r",
        "--workflow=release.yml",
        "--commit",
        "abc123",
        "--limit",
        "1",
        "--json",
        "databaseId",
    ]


# run_watch --------------------------------------------------------------------


def test_run_watch_streams_and_argv(gh_on_path, monkeypatch):
    seen = {}

    def fake_run(cmd, *, capture_output=True, check=False):
        seen["cmd"] = cmd
        seen["capture_output"] = capture_output
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(gh.proc, "run", fake_run)
    gh.run_watch("999", repo="o/r", exit_status=True)
    assert seen["cmd"] == ["gh", "run", "watch", "999", "--repo", "o/r", "--exit-status"]
    assert seen["capture_output"] is False


def test_run_watch_raises_on_failure(gh_on_path, monkeypatch):
    monkeypatch.setattr(gh.proc, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 1))
    with pytest.raises(GhError):
        gh.run_watch("999", repo="o/r", exit_status=True)


# workflow_run -----------------------------------------------------------------


def test_workflow_run_argv(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    # release-cut call site
    res = gh.workflow_run("release.yml", fields={"version": "2.0.0"})
    assert res.returncode == 0
    assert _argv(rec) == ["gh", "workflow", "run", "release.yml", "-f", "version=2.0.0"]


# issue_view / issue_create / issue_close --------------------------------------


def test_issue_view_argv(gh_on_path, monkeypatch):
    rec = _Recorder(stdout='{"number": 1}')
    monkeypatch.setattr(gh.proc, "run", rec)
    # release-notify-source call site
    res = gh.issue_view(
        "12", repo="o/r", json_fields=["number", "title", "url", "body", "comments"]
    )
    assert res.returncode == 0
    assert _argv(rec) == [
        "gh",
        "issue",
        "view",
        "12",
        "--repo",
        "o/r",
        "--json",
        "number,title,url,body,comments",
    ]


def test_issue_create_argv_returns_url(gh_on_path, monkeypatch):
    rec = _Recorder(stdout="https://github.com/o/r/issues/9\n")
    monkeypatch.setattr(gh.proc, "run", rec)
    # gh-release-issue call site
    url = gh.issue_create(repo="o/r", title="T", body="B", label="consumer-filed")
    assert url == "https://github.com/o/r/issues/9"
    assert _argv(rec) == [
        "gh",
        "issue",
        "create",
        "--repo",
        "o/r",
        "--title",
        "T",
        "--body",
        "B",
        "--label",
        "consumer-filed",
    ]


def test_issue_close_argv(gh_on_path, monkeypatch):
    rec = _Recorder()
    monkeypatch.setattr(gh.proc, "run", rec)
    # release-notify-source call site
    gh.issue_close("12", repo="o/r", comment="fixed upstream")
    assert _argv(rec) == [
        "gh",
        "issue",
        "close",
        "12",
        "--repo",
        "o/r",
        "--comment",
        "fixed upstream",
    ]
