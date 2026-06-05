"""Circuit-breaker heuristics + their fold-in to the state machine."""

from __future__ import annotations

from release_core.prstate.breakers import build_cycles, evaluate_breakers
from release_core.prstate.model import PullContext, Review, ReviewComment, Thread
from release_core.prstate.state import TaskState, evaluate


def review(rid: int, sha: str, author: str = "Copilot") -> Review:
    return Review(review_id=rid, author=author, state="COMMENTED", commit_id=sha, body="")


def rc(rid: int, path: str, line: int) -> dict:
    return {"pull_request_review_id": rid, "path": path, "original_line": line}


def ctx(reviews, *, comments=None, threads=None, head=None, mergeable="MERGEABLE", checks=None):
    return PullContext(
        number=1,
        head_sha=head or (reviews[-1].commit_id if reviews else "h"),
        is_draft=True,
        base_ref="main",
        mergeable=mergeable,
        reviews=list(reviews),
        review_comments=comments or [],
        threads=threads or [],
        checks=checks or [],
    )


def open_copilot_thread(path="a.py", line=1):
    comment = ReviewComment(comment_id=1, path=path, line=line, body="x", author="Copilot")
    return Thread(thread_id="PRT_1", is_resolved=False, comments=(comment,))


# --- cycle counting -------------------------------------------------------


def test_build_cycles_one_per_copilot_review_chronological():
    reviews = [review(10, "a"), review(20, "b"), review(5, "c", author="gemini-bot")]
    cycles = build_cycles(ctx(reviews))
    assert [c.index for c in cycles] == [1, 2]
    assert [c.commit_id for c in cycles] == ["a", "b"]  # gemini excluded, id-ordered


def test_cycle_cap_fires_on_fourth():
    reviews = [review(i, f"c{i}") for i in range(1, 5)]
    v = evaluate_breakers(ctx(reviews))
    assert v.stop and v.breaker == "cycle-cap" and v.cycles == 4


def test_three_cycles_under_cap_no_stop():
    reviews = [review(i, f"c{i}") for i in range(1, 4)]
    # disjoint findings each cycle -> no other breaker fires either
    comments = [rc(1, "a.py", 1), rc(2, "b.py", 2), rc(3, "c.py", 3)]
    assert not evaluate_breakers(ctx(reviews, comments=comments)).stop


# --- diff trajectory ------------------------------------------------------


def test_diff_trajectory_growing_stops():
    reviews = [review(1, "c1"), review(2, "c2"), review(3, "c3")]
    sizes = {"c1": 100, "c2": 200, "c3": 410}
    comments = [rc(1, "a", 1), rc(2, "b", 2), rc(3, "c", 3)]  # disjoint, isolate this breaker
    v = evaluate_breakers(ctx(reviews, comments=comments), diff_sizer=sizes.get)
    assert v.stop and v.breaker == "diff-trajectory"


def test_diff_trajectory_shrinking_no_stop():
    reviews = [review(1, "c1"), review(2, "c2"), review(3, "c3")]
    sizes = {"c1": 410, "c2": 200, "c3": 100}
    comments = [rc(1, "a", 1), rc(2, "b", 2), rc(3, "c", 3)]
    assert not evaluate_breakers(ctx(reviews, comments=comments), diff_sizer=sizes.get).stop


def test_diff_trajectory_skipped_without_sizer():
    # No diff_sizer -> diff breaker can't run; only 2 cycles so nothing else fires.
    reviews = [review(1, "c1"), review(2, "c2")]
    comments = [rc(1, "a", 1), rc(2, "b", 2)]
    assert not evaluate_breakers(ctx(reviews, comments=comments)).stop


def test_diff_trajectory_below_floor_no_stop():
    # Growing but tiny (1 -> 2 -> 3 lines) is below MIN_DIFF_LINES -> no false stop.
    reviews = [review(1, "c1"), review(2, "c2"), review(3, "c3")]
    sizes = {"c1": 1, "c2": 2, "c3": 3}
    comments = [rc(1, "a", 1), rc(2, "b", 2), rc(3, "c", 3)]
    assert not evaluate_breakers(ctx(reviews, comments=comments), diff_sizer=sizes.get).stop


# --- comment-set / repeat -------------------------------------------------


def test_comment_fixed_point_identical_stops():
    # Exact same findings two cycles running -> true fixed point.
    reviews = [review(1, "c1"), review(2, "c2")]
    comments = [rc(1, "a.py", 1), rc(1, "b.py", 2), rc(2, "a.py", 1), rc(2, "b.py", 2)]
    v = evaluate_breakers(ctx(reviews, comments=comments))
    assert v.stop and v.breaker == "comment-set"


def test_comment_fixed_point_strict_subset_is_progress_no_stop():
    # cycle2 is a STRICT subset (b.py:2 got resolved) -> progress, not a fixed point.
    reviews = [review(1, "c1"), review(2, "c2")]
    comments = [rc(1, "a.py", 1), rc(1, "b.py", 2), rc(2, "a.py", 1)]
    assert not evaluate_breakers(ctx(reviews, comments=comments)).stop


def test_repeat_finding_three_consecutive_cycles_stops():
    # a.py:1 persists across all 3 cycles (each cycle's set differs, so it's not a
    # fixed point) -> repeat-finding after two failed fix attempts.
    reviews = [review(1, "c1"), review(2, "c2"), review(3, "c3")]
    comments = [
        rc(1, "a.py", 1),
        rc(1, "x.py", 1),
        rc(2, "a.py", 1),
        rc(2, "y.py", 2),
        rc(3, "a.py", 1),
        rc(3, "z.py", 3),
    ]
    v = evaluate_breakers(ctx(reviews, comments=comments))
    assert v.stop and v.breaker == "repeat-finding"


def test_repeat_finding_two_cycles_allows_second_attempt():
    # Same location flagged twice is allowed (a 2nd attempt is normal) -> no stop.
    reviews = [review(1, "c1"), review(2, "c2")]
    comments = [rc(1, "a.py", 1), rc(2, "a.py", 1), rc(2, "c.py", 3)]
    assert not evaluate_breakers(ctx(reviews, comments=comments)).stop


def test_disjoint_consecutive_findings_no_stop():
    reviews = [review(1, "c1"), review(2, "c2")]
    comments = [rc(1, "a.py", 1), rc(2, "b.py", 2)]
    assert not evaluate_breakers(ctx(reviews, comments=comments)).stop


# --- fold-in to state -----------------------------------------------------


def test_breaker_overrides_addressing_with_blocked():
    reviews = [review(i, f"c{i}") for i in range(1, 5)]  # 4 cycles -> cap
    c = ctx(reviews, threads=[open_copilot_thread()], head="c4")
    status = evaluate(c)
    assert status.state is TaskState.BLOCKED
    assert status.breaker == "cycle-cap"
    assert "STOP" in status.next_action


def test_converged_pr_not_stopped_despite_many_cycles():
    # 4 cycles but every thread resolved + green + mergeable -> READY, not BLOCKED.
    reviews = [review(i, f"c{i}") for i in range(1, 5)]
    rollup = [{"status": "COMPLETED", "conclusion": "SUCCESS"}]
    status = evaluate(ctx(reviews, threads=[], head="c4", checks=rollup))
    assert status.state is TaskState.READY
    assert status.cycles == 4
    assert status.breaker is None
