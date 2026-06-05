"""Adapter detection over recorded PR scenarios.

Each test asserts where the Copilot/Gemini adapters place a reviewer in the
lifecycle, exercising the load-bearing rules: head-SHA filtering, the
resolved-thread filter, and Gemini's weak (reaction/comment) signals.
"""

from __future__ import annotations

from release_core.prstate.model import ReviewLifecycle
from release_core.prstate.reviewers import (
    REGISTRY,
    CopilotAdapter,
    GeminiAdapter,
    required_reviewers,
)

COPILOT = CopilotAdapter()
GEMINI = GeminiAdapter()


def test_registry_is_copilot_required_gemini_best_effort():
    assert [r.name for r in REGISTRY] == ["copilot", "gemini"]
    assert [r.name for r in required_reviewers()] == ["copilot"]
    assert COPILOT.required is True
    assert GEMINI.required is False


def test_copilot_done_with_open_comment(context):
    ctx = context("copilot_changes_requested")
    assert COPILOT.detect(ctx) == ReviewLifecycle.DONE_COMMENTS
    assert GEMINI.detect(ctx) == ReviewLifecycle.NOT_REQUESTED
    assert len(COPILOT.open_threads(ctx)) == 1


def test_both_done_clean(context):
    ctx = context("copilot_clean_gemini_clean")
    assert COPILOT.detect(ctx) == ReviewLifecycle.DONE_CLEAN
    assert GEMINI.detect(ctx) == ReviewLifecycle.DONE_CLEAN
    assert ctx.open_threads() == []


def test_gemini_eyes_is_in_progress_copilot_requested(context):
    ctx = context("gemini_eyes_copilot_requested")
    assert GEMINI.detect(ctx) == ReviewLifecycle.IN_PROGRESS
    assert COPILOT.detect(ctx) == ReviewLifecycle.REQUESTED


def test_stale_copilot_review_does_not_count_as_done(context):
    ctx = context("copilot_stale_review")
    # A review against an earlier commit must not read as done on this head.
    assert COPILOT.detect(ctx) == ReviewLifecycle.REQUESTED


def test_gemini_review_on_earlier_head_still_counts_as_done():
    # The exact #345-fixup case: Gemini reviewed the OLD head, a fixup made a new
    # head, and the lingering eyes reaction must NOT downgrade Gemini to
    # in_progress — it reviews once and won't re-review the push.
    from release_core.prstate.model import PullContext, Review

    ctx = PullContext(
        number=1,
        head_sha="new",
        is_draft=True,
        reviews=[Review(1, "gemini-code-assist[bot]", "COMMENTED", "old", "")],
        reactions=[{"content": "eyes", "user": {"login": "gemini-code-assist[bot]"}}],
    )
    assert GEMINI.detect(ctx) == ReviewLifecycle.DONE_CLEAN


def test_copilot_review_on_earlier_head_does_NOT_count_done():
    # Contrast: Copilot is head-strict — a review on an old head is stale.
    from release_core.prstate.model import PullContext, Review

    ctx = PullContext(
        number=1,
        head_sha="new",
        is_draft=True,
        reviews=[Review(1, "Copilot", "COMMENTED", "old", "")],
        requested_logins=["Copilot"],
    )
    assert COPILOT.detect(ctx) == ReviewLifecycle.REQUESTED


def test_dismissed_copilot_review_on_head_does_NOT_count_done():
    # A DISMISSED review (cleared by an admin/author) is retracted — even on the
    # current head it must not read as done; the PR falls back to REQUESTED.
    from release_core.prstate.model import PullContext, Review

    ctx = PullContext(
        number=1,
        head_sha="new",
        is_draft=True,
        reviews=[Review(1, "Copilot", "DISMISSED", "new", "")],
        requested_logins=["Copilot"],
    )
    assert COPILOT.detect(ctx) == ReviewLifecycle.REQUESTED


def test_dismissed_gemini_review_does_NOT_count_done():
    # Same for best-effort Gemini: a dismissed review is not a standing verdict.
    from release_core.prstate.model import PullContext, Review

    ctx = PullContext(
        number=1,
        head_sha="new",
        is_draft=True,
        reviews=[Review(1, "gemini-code-assist[bot]", "DISMISSED", "old", "")],
    )
    assert GEMINI.detect(ctx) == ReviewLifecycle.NOT_REQUESTED


def test_resolved_thread_clears_open_but_keeps_authored(context):
    ctx = context("copilot_done_all_resolved")
    assert COPILOT.detect(ctx) == ReviewLifecycle.DONE_COMMENTS
    assert COPILOT.open_threads(ctx) == []
    assert len(COPILOT.authored_threads(ctx)) == 1
