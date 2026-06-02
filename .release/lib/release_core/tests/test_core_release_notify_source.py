"""release_notify_source verb — blob flattening, PR/repo extraction, body
formatting, and the safety-critical dry-run/--post/--close gating.

The pure functions (extract_pr_urls / extract_reported_repos / build_blob /
notification_body) are tested directly against recorded issue JSON. main()'s
dispatch is exercised by monkeypatching the gh boundary helpers (_view_issue /
_comment_pr / _close_issue) — "mock at the data layer", never the network.
"""

from __future__ import annotations

from release_core import gh
from release_core.verbs import release_notify_source as rns

# Mirrors tests/fleet/release-notify-source.bats's _issue_two_prs fixture:
# a body PR + a duplicate-comment PR.
TWO_PRS = {
    "number": 42,
    "title": "[copilot-review] reviewer empty",
    "url": "https://github.com/arthur-debert/release/issues/42",
    "body": (
        "**Reported from:** arthur-debert/dodot\n"
        "**PR:** https://github.com/arthur-debert/dodot/pull/118\n"
    ),
    "comments": [
        {
            "body": (
                "Also hit on **arthur-debert/clapfig**.\n"
                "- PR: https://github.com/arthur-debert/clapfig/pull/9\n"
            )
        }
    ],
}

NO_PR = {
    "number": 7,
    "title": "[ruleset] x",
    "url": "https://github.com/arthur-debert/release/issues/7",
    "body": "**Reported from:** arthur-debert/padz\n",
    "comments": [],
}


# --- pure extraction / formatting ---


def test_build_blob_flattens_body_then_comments():
    blob = rns.build_blob(TWO_PRS)
    assert "arthur-debert/dodot/pull/118" in blob
    assert "arthur-debert/clapfig/pull/9" in blob
    # body comes before the comment body
    assert blob.index("dodot/pull/118") < blob.index("clapfig/pull/9")


def test_build_blob_tolerates_missing_and_null_bodies():
    assert rns.build_blob({"body": None, "comments": [{"body": None}, {}]}) == "\n\n"
    assert rns.build_blob({}) == ""


def test_extract_pr_urls_distinct_and_sorted():
    blob = rns.build_blob(TWO_PRS)
    assert rns.extract_pr_urls(blob) == [
        "https://github.com/arthur-debert/clapfig/pull/9",
        "https://github.com/arthur-debert/dodot/pull/118",
    ]


def test_extract_pr_urls_dedupes():
    blob = "x https://github.com/a/b/pull/1 y https://github.com/a/b/pull/1 z"
    assert rns.extract_pr_urls(blob) == ["https://github.com/a/b/pull/1"]


def test_extract_pr_urls_empty_when_none():
    assert rns.extract_pr_urls("no links here") == []


def test_extract_reported_repos_bold_and_also_hit():
    blob = rns.build_blob(TWO_PRS)
    repos = rns.extract_reported_repos(blob)
    assert "arthur-debert/dodot" in repos
    assert "arthur-debert/clapfig" in repos


def test_extract_reported_repos_no_pr_case():
    blob = rns.build_blob(NO_PR)
    assert rns.extract_reported_repos(blob) == ["arthur-debert/padz"]


def test_notification_body_shape():
    body = rns.notification_body(42, "https://x/42", "reviewer empty", "release#371")
    assert "Upstream fix shipped" in body
    assert "arthur-debert/release#42" in body
    assert "(https://x/42)" in body
    assert "_reviewer empty_" in body
    assert "**Fix:** release#371" in body
    assert "@v2" in body


# --- main() dispatch / usage / gating, gh mocked at the data layer ---


def test_missing_issue_exits_64(capsys):
    assert rns.main(["--fix", "x"]) == 64


def test_missing_fix_exits_64(capsys):
    assert rns.main(["42"]) == 64
    assert "--fix is required" in capsys.readouterr().err


def test_non_numeric_issue_exits_64(capsys):
    assert rns.main(["not-a-number", "--fix", "x"]) == 64
    assert "must be a number" in capsys.readouterr().err


def test_fix_without_value_exits_64(capsys):
    assert rns.main(["42", "--fix"]) == 64
    assert "--fix needs a value" in capsys.readouterr().err


def test_repo_without_value_exits_64(capsys):
    assert rns.main(["42", "--repo"]) == 64
    assert "--repo needs a value" in capsys.readouterr().err


def test_unknown_arg_exits_64(capsys):
    assert rns.main(["42", "--fix", "x", "--nope"]) == 64
    assert "unknown argument" in capsys.readouterr().err


def test_too_many_positionals_exits_64(capsys):
    assert rns.main(["42", "43", "--fix", "x"]) == 64
    assert "too many positional" in capsys.readouterr().err


def test_help_exits_0(capsys):
    assert rns.main(["--help"]) == 0
    assert "Usage:" in capsys.readouterr().out


def test_view_failure_exits_2(monkeypatch, capsys):
    def boom(*a, **k):
        raise gh.GhError("auth")

    monkeypatch.setattr(rns, "_view_issue", boom)
    assert rns.main(["42", "--fix", "x"]) == 2
    assert "could not read" in capsys.readouterr().err


def test_dry_run_lists_prs_and_posts_nothing(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(TWO_PRS))
    posted: list = []
    monkeypatch.setattr(rns, "_comment_pr", lambda *a, **k: posted.append(a) or True)
    monkeypatch.setattr(rns, "_close_issue", lambda *a, **k: posted.append(a) or True)

    rc = rns.main(["42", "--fix", "release#371"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "DRY-RUN" in out
    assert "dodot/pull/118" in out
    assert "clapfig/pull/9" in out
    assert "release#371" in out
    assert posted == []  # nothing posted in dry-run


def test_dry_run_close_announces_but_does_not_close(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(TWO_PRS))
    closed: list = []
    monkeypatch.setattr(rns, "_close_issue", lambda *a, **k: closed.append(a) or True)
    rc = rns.main(["42", "--fix", "x", "--close"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "(--close) would then close" in out
    assert closed == []


def test_post_comments_each_pr(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(TWO_PRS))
    commented: list = []
    monkeypatch.setattr(rns, "_comment_pr", lambda url, body: commented.append(url) or True)
    rc = rns.main(["42", "--fix", "release#371", "--post"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "notified:" in out
    assert commented == [
        "https://github.com/arthur-debert/clapfig/pull/9",
        "https://github.com/arthur-debert/dodot/pull/118",
    ]


def test_post_comment_failure_exits_1(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(TWO_PRS))
    monkeypatch.setattr(rns, "_comment_pr", lambda url, body: False)
    closed: list = []
    monkeypatch.setattr(rns, "_close_issue", lambda *a, **k: closed.append(a) or True)
    rc = rns.main(["42", "--fix", "x", "--post", "--close"])
    assert rc == 1
    assert "FAILED to comment" in capsys.readouterr().err
    assert closed == []  # --close skipped when any comment failed (rc != 0)


def test_post_close_closes_when_all_succeed(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(TWO_PRS))
    monkeypatch.setattr(rns, "_comment_pr", lambda url, body: True)
    closed: list = []
    monkeypatch.setattr(
        rns, "_close_issue", lambda issue, repo, comment: closed.append((issue, comment)) or True
    )
    rc = rns.main(["42", "--fix", "release#371", "--post", "--close"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "closed:" in out
    assert closed[0][0] == "42"
    assert "Closed by fleet-triage: release#371." in closed[0][1]
    assert "dodot/pull/118" in closed[0][1]


def test_no_source_pr_exits_3_names_reported_repo(monkeypatch, capsys):
    monkeypatch.setattr(rns, "_view_issue", lambda *a, **k: dict(NO_PR))
    posted: list = []
    monkeypatch.setattr(rns, "_comment_pr", lambda *a, **k: posted.append(a) or True)
    rc = rns.main(["7", "--fix", "release#999"])
    err = capsys.readouterr().err
    assert rc == 3
    assert "points at no source PR" in err
    assert "arthur-debert/padz" in err
    assert posted == []
