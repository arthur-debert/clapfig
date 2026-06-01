"""release_inbox verb — clustering, recurrence, render. Pure data layer.

The clustering oracle (release_inbox.cluster) is tested directly against
recorded issue JSON — no gh, no subprocess. main() is exercised by monkeypatching
gh.issue_list, mirroring the contract's "mock at the data layer" rule.
"""

from __future__ import annotations

import datetime

from release_core import gh
from release_core.verbs import release_inbox

NOW = datetime.datetime(2026, 5, 31, tzinfo=datetime.UTC)

# Three issues mirroring tests/fleet/release-inbox.bats's fixture:
# two under copilot-review (3 + 0 comments → recurrence 5), one under
# rust-cli-release (1 comment → recurrence 2).
THREE = [
    {
        "number": 42,
        "title": "[copilot-review] reviewer empty after success",
        "body": "**Reported from:** arthur-debert/dodot\n",
        "createdAt": "2026-05-25T00:00:00Z",
        "url": "https://x/42",
        "comments": [{}, {}, {}],
    },
    {
        "number": 51,
        "title": "[copilot-review] not attached on fork PR",
        "body": "**Reported from:** arthur-debert/clapfig\n",
        "createdAt": "2026-05-29T00:00:00Z",
        "url": "https://x/51",
        "comments": [],
    },
    {
        "number": 48,
        "title": "[rust-cli-release] cargo publish failed at v1.4.2",
        "body": "Reported from: arthur-debert/padz",
        "createdAt": "2026-05-28T00:00:00Z",
        "url": "https://x/48",
        "comments": [{}],
    },
]


def test_cluster_groups_and_sorts_by_recurrence():
    clusters = release_inbox.cluster(THREE, now=NOW)
    assert clusters[0]["component"] == "copilot-review"
    assert clusters[0]["recurrence"] == 5  # (3 + 0 comments) + 2 issues
    assert clusters[0]["issue_count"] == 2
    assert clusters[1]["component"] == "rust-cli-release"
    assert clusters[1]["recurrence"] == 2  # 1 comment + 1 issue


def test_issues_within_cluster_sorted_by_comments_then_age():
    clusters = release_inbox.cluster(THREE, now=NOW)
    issues = clusters[0]["issues"]
    assert [i["number"] for i in issues] == [42, 51]  # 3 comments before 0


def test_source_repo_parsed_from_bold_and_bare_forms():
    clusters = release_inbox.cluster(THREE, now=NOW)
    by_num = {i["number"]: i for c in clusters for i in c["issues"]}
    assert by_num[42]["repo"] == "arthur-debert/dodot"  # **Reported from:**
    assert by_num[48]["repo"] == "arthur-debert/padz"  # bare Reported from:


def test_symptom_strips_component_prefix():
    clusters = release_inbox.cluster(THREE, now=NOW)
    by_num = {i["number"]: i for c in clusters for i in c["issues"]}
    assert by_num[42]["symptom"] == "reviewer empty after success"


def test_age_days_floor():
    clusters = release_inbox.cluster(THREE, now=NOW)
    by_num = {i["number"]: i for c in clusters for i in c["issues"]}
    assert by_num[42]["age_days"] == 6  # 2026-05-25 → 2026-05-31


def test_untagged_issue_lands_under_other():
    issue = [
        {
            "number": 9,
            "title": "untagged escalation",
            "body": "",
            "createdAt": "2026-05-29T00:00:00Z",
            "url": "https://x/9",
            "comments": [],
        }
    ]
    clusters = release_inbox.cluster(issue, now=NOW)
    assert clusters[0]["component"] == "other"


def test_missing_body_yields_unknown_repo():
    issue = [
        {
            "number": 7,
            "title": "[x] y",
            "body": None,
            "createdAt": "2026-05-29T00:00:00Z",
            "url": "https://x/7",
            "comments": [],
        }
    ]
    clusters = release_inbox.cluster(issue, now=NOW)
    assert clusters[0]["issues"][0]["repo"] == "unknown"


def test_empty_inbox_clusters_to_nothing():
    assert release_inbox.cluster([], now=NOW) == []


def test_render_human_empty_is_clear():
    out = release_inbox.render_human([], "consumer-filed")
    assert "no open" in out
    assert "consumer-filed" in out


def test_render_human_shows_repo_comments_url():
    clusters = release_inbox.cluster(THREE, now=NOW)
    out = release_inbox.render_human(clusters, "consumer-filed")
    assert "arthur-debert/dodot" in out
    assert "3 comment(s)" in out
    assert "https://x/42" in out


# --- main() dispatch, gh boundary mocked at the data layer ---


def test_main_json_outputs_clusters(monkeypatch, capsys):
    monkeypatch.setattr(gh, "issue_list", lambda *a, **k: list(THREE))
    rc = release_inbox.main(["--json"])
    assert rc == 0
    import json

    data = json.loads(capsys.readouterr().out)
    assert data[0]["component"] == "copilot-review"


def test_main_human_default(monkeypatch, capsys):
    monkeypatch.setattr(gh, "issue_list", lambda *a, **k: list(THREE))
    rc = release_inbox.main([])
    assert rc == 0
    assert "Fleet inbox —" in capsys.readouterr().out


def test_main_empty_human(monkeypatch, capsys):
    monkeypatch.setattr(gh, "issue_list", lambda *a, **k: [])
    rc = release_inbox.main([])
    assert rc == 0
    assert "no open" in capsys.readouterr().out


def test_main_gh_error_exits_2(monkeypatch, capsys):
    def boom(*a, **k):
        raise gh.GhError("auth")

    monkeypatch.setattr(gh, "issue_list", boom)
    rc = release_inbox.main([])
    assert rc == 2
    assert "could not read issues" in capsys.readouterr().err


def test_main_label_passed_to_gh(monkeypatch):
    seen = {}

    def capture(repo, **kw):
        seen.update(repo=repo, **kw)
        return []

    monkeypatch.setattr(gh, "issue_list", capture)
    release_inbox.main(["--label", "my-label", "--repo", "o/r"])
    assert seen["label"] == "my-label"
    assert seen["repo"] == "o/r"


def test_main_label_without_value_exits_64(monkeypatch, capsys):
    rc = release_inbox.main(["--label"])
    assert rc == 64
    assert "--label needs a value" in capsys.readouterr().err


def test_main_unknown_arg_exits_64(capsys):
    rc = release_inbox.main(["--nope"])
    assert rc == 64


def test_main_help_exits_0(capsys):
    rc = release_inbox.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out
