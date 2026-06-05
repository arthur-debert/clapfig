"""Model-level invariants: thread accessors and head/resolved filtering."""

from __future__ import annotations

from release_core.prstate.model import PullContext, Review, ReviewComment, Thread


def _thread(thread_id, resolved, *comments):
    return Thread(thread_id=thread_id, is_resolved=resolved, comments=tuple(comments))


def test_thread_location_comes_from_root_comment():
    root = ReviewComment(comment_id=1, path="a.py", line=10, body="x", author="Copilot")
    reply = ReviewComment(comment_id=2, path="a.py", line=10, body="ok", author="me")
    t = _thread("PRT_1", False, root, reply)
    assert t.path == "a.py"
    assert t.line == 10
    assert t.root_comment_id == 1
    assert t.author == "Copilot"


def test_empty_thread_has_no_location():
    t = _thread("PRT_empty", False)
    assert t.path is None
    assert t.line is None
    assert t.root_comment_id is None


def test_reviews_on_head_filters_stale():
    ctx = PullContext(
        number=1,
        head_sha="head",
        is_draft=True,
        reviews=[
            Review(1, "Copilot", "COMMENTED", "head", ""),
            Review(2, "Copilot", "COMMENTED", "stale", ""),
        ],
    )
    assert [r.review_id for r in ctx.reviews_on_head()] == [1]


def test_open_threads_excludes_resolved():
    ctx = PullContext(
        number=1,
        head_sha="head",
        is_draft=True,
        threads=[_thread("a", False), _thread("b", True)],
    )
    assert [t.thread_id for t in ctx.open_threads()] == ["a"]
