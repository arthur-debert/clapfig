"""Circuit breakers — detect a diverging review loop and STOP before iterating.

Heuristics from docs/proposals/pr-review-loop-circuit-breakers.md. A *cycle* is
one Copilot review; its findings are the inline comments attached to that
review. All inputs derive from gh review history, except diff sizes, which come
from git and are injected via `diff_sizer` — omitted in pure evaluation, in
which case the diff-trajectory breaker is skipped rather than guessed.

The verdict folds into the state machine as the STOP form of BLOCKED, and only
when the loop would otherwise iterate (open threads remain). A converged PR is
never stopped.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

from .model import PullContext

CYCLE_CAP = 3
DIFF_GROWTH_TOLERANCE = 1.1  # allow 10% jitter before calling it "growing"
MIN_DIFF_LINES = 50  # below this the diff is too small for "growing" to mean anything
REPEAT_WINDOW = 3  # a location must persist this many consecutive cycles to stop

CommentKey = tuple[str, "int | None"]
DiffSizer = Callable[[str], "int | None"]


@dataclass(frozen=True)
class Cycle:
    index: int  # 1-based, chronological
    commit_id: str
    comment_keys: frozenset  # {(path, line)} of this review's findings
    diff_size: int | None = None


@dataclass(frozen=True)
class BreakerVerdict:
    stop: bool
    breaker: str | None
    reason: str
    cycles: int


def _is_copilot(login: str) -> bool:
    return "copilot" in login.lower()


def build_cycles(ctx: PullContext, diff_sizer: DiffSizer | None = None) -> list[Cycle]:
    """One Cycle per Copilot review, chronological, with its finding keys."""
    reviews = sorted((r for r in ctx.reviews if _is_copilot(r.author)), key=lambda r: r.review_id)
    cycles: list[Cycle] = []
    for index, review in enumerate(reviews, start=1):
        keys = frozenset(
            (c.get("path") or "", c.get("original_line") or c.get("line"))
            for c in ctx.review_comments
            if c.get("pull_request_review_id") == review.review_id
        )
        size = diff_sizer(review.commit_id) if diff_sizer else None
        cycles.append(Cycle(index, review.commit_id, keys, size))
    return cycles


def evaluate_breakers(ctx: PullContext, diff_sizer: DiffSizer | None = None) -> BreakerVerdict:
    """Run the breaker stack (priority order); first to fire wins."""
    cycles = build_cycles(ctx, diff_sizer)
    n = len(cycles)

    if n > CYCLE_CAP:
        return BreakerVerdict(
            True, "cycle-cap", f"{n} review cycles exceeds the cap of {CYCLE_CAP}", n
        )

    for check in (_diff_trajectory, _comment_fixed_point, _repeat_finding):
        verdict = check(cycles, n)
        if verdict is not None:
            return verdict

    return BreakerVerdict(False, None, "", n)


def _diff_trajectory(cycles: list[Cycle], n: int) -> BreakerVerdict | None:
    """Diff growing two consecutive cycles signals divergence (would catch #118).

    Guarded by an absolute floor so a 1 -> 2 -> 3 line PR doesn't trip it — only
    diffs that are both growing *and* non-trivial count.
    """
    sized = [c for c in cycles if c.diff_size is not None]
    if len(sized) < 3:
        return None
    a, b, c = sized[-3], sized[-2], sized[-1]
    if c.diff_size < MIN_DIFF_LINES:
        return None
    if b.diff_size > a.diff_size * DIFF_GROWTH_TOLERANCE and (
        c.diff_size > b.diff_size * DIFF_GROWTH_TOLERANCE
    ):
        return BreakerVerdict(
            True,
            "diff-trajectory",
            f"diff growing across cycles: {a.diff_size} -> {b.diff_size} -> {c.diff_size}",
            n,
        )
    return None


def _comment_fixed_point(cycles: list[Cycle], n: int) -> BreakerVerdict | None:
    """Cycle N+1's findings are *identical* to cycle N's — a true fixed point.

    Equality, not subset: a strict subset means some findings were resolved,
    which is progress, not a stuck loop. A persisting location is the
    repeat-finding breaker's job, not this one's.
    """
    if n < 2:
        return None
    prev, last = cycles[-2], cycles[-1]
    if last.comment_keys and last.comment_keys == prev.comment_keys:
        return BreakerVerdict(
            True,
            "comment-set",
            "latest review's findings are identical to the previous cycle's (fixed point)",
            n,
        )
    return None


def _repeat_finding(cycles: list[Cycle], n: int) -> BreakerVerdict | None:
    """The same (path, line) flagged across REPEAT_WINDOW consecutive cycles.

    Requires the location to survive two fix attempts (3 cycles by default), not
    one — a second attempt at a stubborn comment is normal; a third is the
    signal that it needs redesign rather than another fix.
    """
    if n < REPEAT_WINDOW:
        return None
    window = cycles[-REPEAT_WINDOW:]
    recurring = set.intersection(*(set(c.comment_keys) for c in window))
    if recurring:
        where = ", ".join(
            f"{p}:{ln}" for p, ln in sorted(recurring, key=lambda k: (k[0], k[1] or 0))
        )
        return BreakerVerdict(
            True,
            "repeat-finding",
            f"same location flagged in {REPEAT_WINDOW} consecutive cycles ({where}) — "
            "redesign, not another fix",
            n,
        )
    return None
