"""Engine behaviour over REAL captured gh payloads (release#337).

These fixtures were recorded live from arthur-debert/release#342 — a throwaway
probe PR driven through actual Copilot + Gemini reviews — not hand-written. They
pin the engine against the real shapes GitHub returns: bot login variants
(`copilot-pull-request-reviewer`, `gemini-code-assist`), the empty
`reviewRequests` even when Copilot is engaged, GraphQL thread node ids, and the
resolved-thread transition to READY.
"""

from __future__ import annotations

from release_core.prstate.state import TaskState, evaluate


def test_live_addressing_real_payload(context):
    status = evaluate(context("live_addressing_pr342"))
    assert status.state is TaskState.ADDRESSING
    # Both bots reviewed and left a comment; real login variants matched.
    assert status.reviewers == {"copilot": "done_comments", "gemini": "done_comments"}
    assert status.open_threads == 2
    assert status.cycles == 1
    assert status.breaker is None


def test_live_ready_real_payload(context):
    # Same PR after replying + resolving both threads — drives to READY.
    status = evaluate(context("live_ready_pr342"))
    assert status.state is TaskState.READY
    assert status.open_threads == 0
    assert status.checks.value == "green"
    assert status.mergeable == "MERGEABLE"
