"""audit_smoke_test verb — pure decision helpers + arg parsing.

The orchestration (clone/commit/push/PR open+close) is genuine side-effecting
glue and is NOT unit-tested (it requires a live repo + GitHub Actions — that is
the script's whole point). What IS pure and tested here: smoke-target selection,
the run/timeline JSON parsing, the report verdict, and arg-parse exit codes.
"""

from __future__ import annotations

from release_core.verbs import audit_smoke_test as smoke

# --------------------------------------------------------------------------
# pick_target
# --------------------------------------------------------------------------


def test_pick_target_prefers_changelog_unreleased(tmp_path):
    (tmp_path / "README.md").write_text("x")
    (tmp_path / "CHANGELOG_UNRELEASED.md").write_text("y")
    assert smoke.pick_target(str(tmp_path)) == "CHANGELOG_UNRELEASED.md"


def test_pick_target_falls_back_to_readme(tmp_path):
    (tmp_path / "README.md").write_text("x")
    assert smoke.pick_target(str(tmp_path)) == "README.md"


def test_pick_target_finds_any_markdown(tmp_path):
    sub = tmp_path / "docs"
    sub.mkdir()
    (sub / "guide.md").write_text("x")
    assert smoke.pick_target(str(tmp_path)) == "docs/guide.md"


def test_pick_target_skips_node_modules(tmp_path):
    nm = tmp_path / "node_modules" / "pkg"
    nm.mkdir(parents=True)
    (nm / "readme.md").write_text("x")
    assert smoke.pick_target(str(tmp_path)) is None


def test_pick_target_none_when_no_markdown(tmp_path):
    (tmp_path / "main.rs").write_text("fn main(){}")
    assert smoke.pick_target(str(tmp_path)) is None


# --------------------------------------------------------------------------
# run / timeline parsing
# --------------------------------------------------------------------------


def test_copilot_run_id_picks_first_copilot_run():
    payload = {
        "workflow_runs": [
            {"name": "CI", "id": 1},
            {"name": "Copilot Review", "id": 42},
            {"name": "Copilot Review", "id": 7},
        ]
    }
    assert smoke.copilot_run_id(payload) == "42"


def test_copilot_run_id_empty_when_absent():
    assert smoke.copilot_run_id({"workflow_runs": [{"name": "CI", "id": 1}]}) == ""
    assert smoke.copilot_run_id(None) == ""


def test_ci_run_names_excludes_copilot_unique_sorted():
    payload = {
        "workflow_runs": [
            {"name": "CI"},
            {"name": "Copilot Review"},
            {"name": "Build"},
            {"name": "CI"},
        ]
    }
    assert smoke.ci_run_names(payload) == "Build,CI"


def test_ci_run_names_empty():
    assert smoke.ci_run_names({"workflow_runs": []}) == ""
    assert smoke.ci_run_names(None) == ""


def test_copilot_requested_counts_copilot_review_requests():
    timeline = [
        {"event": "review_requested", "requested_reviewer": {"login": "Copilot"}},
        {"event": "review_requested", "requested_reviewer": {"login": "someone"}},
        {"event": "commented"},
        {"event": "review_requested", "requested_reviewer": {"login": "Copilot"}},
    ]
    assert smoke.copilot_requested(timeline) == 2


def test_copilot_requested_handles_non_list():
    assert smoke.copilot_requested(None) == 0
    assert smoke.copilot_requested({}) == 0


# --------------------------------------------------------------------------
# render_report verdict
# --------------------------------------------------------------------------


def test_report_all_pass():
    report, fails = smoke.render_report("o/r", "12", "99", 1, "CI,Build")
    assert fails == 0
    assert "smoke test PASSED" in report
    assert "[PASS]" in report
    assert "run=99" in report


def test_report_no_copilot_run_fails():
    report, fails = smoke.render_report("o/r", "12", "", 1, "CI")
    assert fails == 1
    assert "no run within 90s" in report
    assert "smoke test FAILED (1 check(s))" in report


def test_report_no_request_fails():
    report, fails = smoke.render_report("o/r", "12", "99", 0, "CI")
    assert fails == 1
    assert "no review_requested event" in report


def test_report_no_ci_runs_warns_not_fails():
    report, fails = smoke.render_report("o/r", "12", "99", 1, "")
    assert fails == 0
    assert "[WARN]" in report
    assert "no non-copilot workflows" in report


def test_last_int():
    assert smoke._last_int("https://github.com/o/r/pull/123") == "123"
    assert smoke._last_int("https://github.com/o/r/pull/123\n") == "123"
    assert smoke._last_int("no number here") == ""


# --------------------------------------------------------------------------
# arg parsing
# --------------------------------------------------------------------------


def test_no_repo_exits_64(capsys):
    rc = smoke.main([])
    assert rc == 64
    assert "usage:" in capsys.readouterr().err


def test_unknown_flag_exits_64(capsys):
    rc = smoke.main(["--nope"])
    assert rc == 64


def test_extra_positional_exits_64(capsys):
    rc = smoke.main(["o/r", "extra"])
    assert rc == 64


def test_base_without_value_exits_64(capsys):
    rc = smoke.main(["o/r", "--base"])
    assert rc == 64


def test_help_exits_0(capsys):
    rc = smoke.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out
