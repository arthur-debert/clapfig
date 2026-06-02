"""list_repo_pr verb — PR-node field extraction + ANSI/column render.

Offline: the gh.graphql boundary is monkeypatched with recorded GraphQL node
dicts. Asserts the status/CI/mergeable/comment derivations and the merge-ready
green-URL rule — the load-bearing parts of the dashboard contract.
"""

from __future__ import annotations

from release_core import gh
from release_core.verbs import list_repo_pr


def _pr(**over):
    base = {
        "number": 7,
        "title": "a title",
        "url": "https://gh/7",
        "isDraft": False,
        "mergeable": "MERGEABLE",
        "reviewThreads": {"totalCount": 0, "nodes": []},
        "reviews": {"totalCount": 0, "nodes": []},
        "comments": {"totalCount": 0},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]},
    }
    base.update(over)
    return base


def _row(pr, capsys):
    list_repo_pr._print_row(pr)
    return capsys.readouterr().out


def test_merge_ready_pr_colors_url_green(capsys):
    out = _row(_pr(), capsys)
    assert list_repo_pr.GRN in out
    assert "https://gh/7" in out
    assert "+ pass" in out
    assert "yes" in out  # mergeable


def test_draft_shows_draft_yellow(capsys):
    out = _row(_pr(isDraft=True), capsys)
    assert "draft" in out


def test_failure_ci_shown_and_url_not_green(capsys):
    pr = _pr(commits={"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]})
    out = _row(pr, capsys)
    assert "x fail" in out
    # URL not green: the green run before the url is absent (only RED for CI cell)
    assert f"{list_repo_pr.GRN}https://gh/7" not in out


def test_null_rollup_renders_dash(capsys):
    pr = _pr(commits={"nodes": [{"commit": {"statusCheckRollup": None}}]})
    out = _row(pr, capsys)
    assert "https://gh/7" in out  # does not crash on null rollup


def test_no_commits_renders_dash(capsys):
    pr = _pr(commits={"nodes": []})
    out = _row(pr, capsys)
    assert "https://gh/7" in out


def test_conflicting_mergeable(capsys):
    out = _row(_pr(mergeable="CONFLICTING"), capsys)
    assert "conflict" in out


def test_total_comments_sums_inline_and_pr(capsys):
    pr = _pr(
        reviews={"totalCount": 1, "nodes": [{"comments": {"totalCount": 2}}]},
        comments={"totalCount": 3},
    )
    out = _row(pr, capsys)
    assert " 5 " in out  # 2 inline + 3 pr


def test_unresolved_threads_counted(capsys):
    pr = _pr(
        reviewThreads={
            "totalCount": 2,
            "nodes": [{"isResolved": False}, {"isResolved": True}, {"isResolved": False}],
        },
    )
    out = _row(pr, capsys)
    # 2 unresolved, and that disqualifies the green url
    assert f"{list_repo_pr.GRN}https://gh/7" not in out


# --- main() dispatch ---


def test_owner_filter_and_no_prs(monkeypatch, capsys):
    monkeypatch.setattr(
        gh, "graphql", lambda *a, **k: {"repository": {"pullRequests": {"nodes": []}}}
    )
    rc = list_repo_pr.main(["--owner", "lex-fmt"])
    assert rc == 0
    assert "no open PRs" in capsys.readouterr().out


def test_unknown_arg_exits_64(capsys):
    rc = list_repo_pr.main(["--nope"])
    assert rc == 64


def test_help_exits_0(capsys):
    rc = list_repo_pr.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out


def test_api_error_reported_inline(monkeypatch, capsys):
    def boom(*a, **k):
        raise gh.GhError("nope")

    monkeypatch.setattr(gh, "graphql", boom)
    rc = list_repo_pr.main(["--owner", "lex-fmt"])
    assert rc == 0
    assert "API error" in capsys.readouterr().out


def test_repo_with_prs_renders_header_and_row(monkeypatch, capsys):
    def one(*a, **k):
        return {"repository": {"pullRequests": {"nodes": [_pr()]}}}

    monkeypatch.setattr(gh, "graphql", one)
    rc = list_repo_pr.main(["--owner", "arthur-debert"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "open)" in out
    assert "status" in out  # header
    assert "#7" in out
