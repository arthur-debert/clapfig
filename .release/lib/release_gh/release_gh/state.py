"""The PR lifecycle state machine — the stable core.

`evaluate()` is a pure function from a `PullContext` snapshot to one
`TaskStatus`: where the PR stands and the single next action. It never mutates
(it *reports* READY; the caller does the draft->ready flip) and never branches
on a reviewer's name — it consumes the adapter interface only.

Two definitions anchor it (see docs/proposals/dev-workflow-state-engine.lex):
  Reviewed = every required reviewer done + every thread resolved.
  Ready    = Reviewed + CI green + mergeable.

Best-effort reviewers (Gemini) never gate: an absent or in-progress best-effort
reviewer does not hold the PR in REVIEWS_PENDING. The *skip-after-timeout*
decision is the polling caller's, not the snapshot's — the snapshot is
stateless and has no clock.

One review cycle is assumed (the first addressing is not itself re-reviewed);
the structure leaves room to add cycles later.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum

from .breakers import DiffSizer, evaluate_breakers
from .model import PullContext, ReviewLifecycle
from .reviewers import REGISTRY, ReviewerAdapter

_DONE = {ReviewLifecycle.DONE_CLEAN, ReviewLifecycle.DONE_COMMENTS}

# CheckRun conclusions / StatusContext states that count as failures.
_FAIL_CONCLUSIONS = {"FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE"}
_FAIL_STATES = {"FAILURE", "ERROR"}
_PENDING_STATUSES = {"QUEUED", "IN_PROGRESS", "PENDING", "WAITING", "REQUESTED", "EXPECTED"}


class TaskState(StrEnum):
    NO_PR = "no_pr"
    REVIEWS_PENDING = "reviews_pending"
    ADDRESSING = "addressing"
    REVIEWED = "reviewed"
    VALIDATING = "validating"
    READY = "ready"
    BLOCKED = "blocked"


class ChecksState(StrEnum):
    NONE = "none"  # no checks configured
    GREEN = "green"
    PENDING = "pending"
    FAILING = "failing"


@dataclass
class TaskStatus:
    """The snapshot result: lifecycle position + the one next action."""

    state: TaskState
    next_action: str
    pr: int | None = None
    reviewers: dict[str, str] = field(default_factory=dict)
    open_threads: int = 0
    checks: ChecksState = ChecksState.NONE
    mergeable: str | None = None
    cycles: int = 0  # completed Copilot review cycles
    breaker: str | None = None  # which circuit breaker fired, if any

    def to_dict(self) -> dict:
        return {
            "pr": self.pr,
            "state": self.state.value,
            "next_action": self.next_action,
            "reviewers": self.reviewers,
            "open_threads": self.open_threads,
            "checks": self.checks.value,
            "mergeable": self.mergeable,
            "cycles": self.cycles,
            "breaker": self.breaker,
        }


def no_pr() -> TaskStatus:
    """No PR exists for the branch — the entry state."""
    return TaskStatus(
        state=TaskState.NO_PR,
        next_action="no PR for this branch — create a draft PR to start the review loop",
    )


def evaluate(
    ctx: PullContext,
    registry: list[ReviewerAdapter] | None = None,
    diff_sizer: DiffSizer | None = None,
) -> TaskStatus:
    """Compute the PR's lifecycle state from a snapshot.

    Pure except for `diff_sizer`, an optional git-backed callable for the
    diff-trajectory breaker; without it that one breaker is skipped.
    """
    registry = registry if registry is not None else REGISTRY
    lifecycles = {r.name: r.detect(ctx) for r in registry}
    reviewers = {name: lc.value for name, lc in lifecycles.items()}
    open_threads = len(ctx.open_threads())
    checks = classify_checks(ctx.checks)
    breaker = evaluate_breakers(ctx, diff_sizer)

    status = TaskStatus(
        state=TaskState.REVIEWS_PENDING,  # provisional; set below
        next_action="",
        pr=ctx.number,
        reviewers=reviewers,
        open_threads=open_threads,
        checks=checks,
        mergeable=ctx.mergeable,
        cycles=breaker.cycles,
    )

    # 1. Required reviewers must all be done. Best-effort ones never gate.
    pending_required = [r.name for r in registry if r.required and lifecycles[r.name] not in _DONE]
    if pending_required:
        status.state = TaskState.REVIEWS_PENDING
        status.next_action = (
            f"waiting on required review(s): {', '.join(pending_required)} — "
            "request if not yet requested, else wait"
        )
        return status

    # 2. Required reviews in; any open thread (from any reviewer) must be addressed
    #    — UNLESS a circuit breaker says the loop is diverging: then STOP, don't
    #    open another cycle. A converged PR (no open threads) is never stopped.
    if open_threads:
        if breaker.stop:
            status.state = TaskState.BLOCKED
            status.breaker = breaker.breaker
            status.next_action = (
                f"STOP — circuit breaker '{breaker.breaker}' fired: {breaker.reason}. "
                "Do not iterate; surface to the human."
            )
            return status
        status.state = TaskState.ADDRESSING
        status.next_action = (
            f"triage {open_threads} open thread(s): fix-or-reply, then resolve each"
        )
        return status

    # 3. Reviewed. Now gate on mergeability + CI.
    if ctx.mergeable == "CONFLICTING":
        status.state = TaskState.BLOCKED
        status.next_action = "merge conflict — rebase/resolve against the base branch"
        return status

    if checks == ChecksState.FAILING:
        status.state = TaskState.BLOCKED
        status.next_action = "CI check(s) failing — fix and push before this can be Ready"
        return status

    if checks == ChecksState.PENDING:
        status.state = TaskState.VALIDATING
        status.next_action = "reviews done; CI check(s) running — wait for checks"
        return status

    if ctx.mergeable == "MERGEABLE":
        status.state = TaskState.READY
        status.next_action = (
            "reviewed + CI green + mergeable — flip draft->ready and page the human"
        )
        return status

    # Reviewed, but mergeability still unknown (GitHub computing) — re-poll.
    status.state = TaskState.REVIEWED
    status.next_action = "reviews done; mergeability not yet determined — re-check shortly"
    return status


def classify_checks(rollup: list[dict]) -> ChecksState:
    """Reduce a gh `statusCheckRollup` to one state.

    Handles both CheckRun entries (status/conclusion) and legacy StatusContext
    entries (state). Failing dominates pending dominates green.
    """
    if not rollup:
        return ChecksState.NONE
    saw_pending = False
    saw_green = False
    for entry in rollup:
        if _is_failing(entry):
            return ChecksState.FAILING
        if _is_pending(entry):
            saw_pending = True
        else:
            saw_green = True
    if saw_pending:
        return ChecksState.PENDING
    return ChecksState.GREEN if saw_green else ChecksState.NONE


def _is_failing(entry: dict) -> bool:
    if entry.get("conclusion") in _FAIL_CONCLUSIONS:
        return True
    return entry.get("state") in _FAIL_STATES


def _is_pending(entry: dict) -> bool:
    # CheckRun: any status other than COMPLETED is still running.
    # StatusContext (no `status` field): a pending-ish `state`.
    status = entry.get("status")
    if status is not None:
        return status != "COMPLETED"
    return entry.get("state") in _PENDING_STATUSES
