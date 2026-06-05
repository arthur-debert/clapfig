"""State machine: scenario -> TaskState, plus check-rollup classification."""

from __future__ import annotations

import pytest
from release_core.prstate.state import (
    ChecksState,
    TaskState,
    classify_checks,
    evaluate,
    no_pr,
)


def test_no_pr():
    status = no_pr()
    assert status.state is TaskState.NO_PR
    assert "create a draft PR" in status.next_action


@pytest.mark.parametrize(
    ("fixture", "expected"),
    [
        ("gemini_eyes_copilot_requested", TaskState.REVIEWS_PENDING),
        ("copilot_stale_review", TaskState.REVIEWS_PENDING),
        ("copilot_changes_requested", TaskState.ADDRESSING),
        ("reviewed_mergeable_unknown", TaskState.REVIEWED),
        ("validating_checks_pending", TaskState.VALIDATING),
        ("ready_checks_green", TaskState.READY),
        ("copilot_clean_gemini_clean", TaskState.READY),
        ("copilot_done_all_resolved", TaskState.READY),
        ("blocked_checks_failing", TaskState.BLOCKED),
        ("blocked_merge_conflict", TaskState.BLOCKED),
    ],
)
def test_evaluate_states(context, fixture, expected):
    assert evaluate(context(fixture)).state is expected


def test_best_effort_gemini_does_not_gate_ready(context):
    # Gemini is NOT_REQUESTED here, yet Copilot (required) is done clean with
    # green checks -> READY. A best-effort reviewer must not hold it back.
    status = evaluate(context("ready_checks_green"))
    assert status.state is TaskState.READY
    assert status.reviewers["gemini"] == "done_clean"


def test_addressing_reports_open_thread_count(context):
    status = evaluate(context("copilot_changes_requested"))
    assert status.state is TaskState.ADDRESSING
    assert status.open_threads == 1
    assert "1 open thread" in status.next_action


def test_blocked_reasons_are_distinct(context):
    assert "conflict" in evaluate(context("blocked_merge_conflict")).next_action
    assert "failing" in evaluate(context("blocked_checks_failing")).next_action


def test_status_to_dict_round_trips(context):
    d = evaluate(context("ready_checks_green")).to_dict()
    assert d["state"] == "ready"
    assert d["checks"] == "green"
    assert d["mergeable"] == "MERGEABLE"
    assert set(d) == {
        "pr",
        "state",
        "next_action",
        "reviewers",
        "open_threads",
        "checks",
        "mergeable",
        "cycles",
        "breaker",
    }


# --- REVIEWS_PENDING next-action wording (request vs re-request vs wait) ----


def test_reviews_pending_never_requested_says_request(context):
    # No review ever landed and Copilot is not requested → the action is to
    # REQUEST (not wait), and it must NOT mention re-request/stale.
    status = evaluate(context("copilot_never_requested"))
    assert status.state is TaskState.REVIEWS_PENDING
    assert "request for the current head" in status.next_action
    assert "copilot" in status.next_action  # the reviewer is named in the clause
    assert "RE-REQUEST" not in status.next_action
    assert "stale" not in status.next_action


def test_reviews_pending_stale_after_push_says_rerequest(context):
    # Copilot reviewed an EARLIER commit; a push has moved the head and reset the
    # request to not_requested. The action must distinguish this from a fresh
    # request: RE-REQUEST for the current head, and name the staleness.
    status = evaluate(context("copilot_stale_needs_rerequest"))
    assert status.state is TaskState.REVIEWS_PENDING
    assert "RE-REQUEST for the current head" in status.next_action
    assert "stale after a push" in status.next_action
    assert "copilot" in status.next_action


def test_reviews_pending_already_requested_says_wait(context):
    # Copilot is REQUESTED on the current head (no review yet) → just wait; the
    # action must not tell the caller to (re-)request what is already pending.
    status = evaluate(context("gemini_eyes_copilot_requested"))
    assert status.state is TaskState.REVIEWS_PENDING
    assert "wait (already requested on the current head)" in status.next_action
    assert "RE-REQUEST" not in status.next_action


# --- classify_checks ------------------------------------------------------


def test_classify_empty_is_none():
    assert classify_checks([]) is ChecksState.NONE


def test_classify_all_success_is_green():
    rollup = [
        {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
        {"__typename": "StatusContext", "state": "SUCCESS"},
    ]
    assert classify_checks(rollup) is ChecksState.GREEN


def test_classify_pending_beats_green():
    rollup = [
        {"status": "COMPLETED", "conclusion": "SUCCESS"},
        {"status": "IN_PROGRESS", "conclusion": None},
    ]
    assert classify_checks(rollup) is ChecksState.PENDING


def test_classify_failing_beats_everything():
    rollup = [
        {"status": "IN_PROGRESS", "conclusion": None},
        {"status": "COMPLETED", "conclusion": "FAILURE"},
    ]
    assert classify_checks(rollup) is ChecksState.FAILING


def test_classify_status_context_error_is_failing():
    rollup = [{"__typename": "StatusContext", "state": "ERROR"}]
    assert classify_checks(rollup) is ChecksState.FAILING


def test_classify_expected_status_is_pending():
    # EXPECTED = a status that's expected but hasn't reported yet -> not green.
    rollup = [{"__typename": "StatusContext", "state": "EXPECTED"}]
    assert classify_checks(rollup) is ChecksState.PENDING


def test_classify_neutral_and_skipped_are_green():
    rollup = [
        {"status": "COMPLETED", "conclusion": "NEUTRAL"},
        {"status": "COMPLETED", "conclusion": "SKIPPED"},
    ]
    assert classify_checks(rollup) is ChecksState.GREEN
