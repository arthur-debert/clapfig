"""orc watch — transition dispatch + poll dedup (pure; no SDK, no gh)."""

from __future__ import annotations

from dataclasses import dataclass, field

from orchestrator.watch import Action, Sink, build_fixer_prompt, decide, poll_once, run
from release_core.prstate.state import ChecksState, TaskState, TaskStatus


def status(state: TaskState, *, breaker: str | None = None) -> TaskStatus:
    return TaskStatus(
        state=state,
        next_action="",
        pr=1,
        checks=ChecksState.GREEN,
        breaker=breaker,
    )


# --- decide() -------------------------------------------------------------


def test_ready_flips_and_pages_not_auto_mergeable():
    assert decide(status(TaskState.READY), auto=True) is Action.FLIP_READY
    assert decide(status(TaskState.READY), auto=False) is Action.FLIP_READY


def test_breaker_always_pages_never_acts():
    s = status(TaskState.BLOCKED, breaker="cycle-cap")
    assert decide(s, auto=True) is Action.PAGE_BREAKER
    assert decide(s, auto=False) is Action.PAGE_BREAKER


def test_addressing_spawns_in_auto_notifies_otherwise():
    assert decide(status(TaskState.ADDRESSING), auto=True) is Action.SPAWN_FIXER
    assert decide(status(TaskState.ADDRESSING), auto=False) is Action.NOTIFY


def test_blocked_without_breaker_is_a_fixable_block():
    s = status(TaskState.BLOCKED)  # failing check / conflict, no breaker
    assert decide(s, auto=True) is Action.SPAWN_FIXER
    assert decide(s, auto=False) is Action.NOTIFY


def test_wait_states_do_nothing():
    for st in (TaskState.REVIEWS_PENDING, TaskState.VALIDATING, TaskState.REVIEWED):
        assert decide(status(st), auto=True) is Action.WAIT


# --- poll_once() ----------------------------------------------------------


@dataclass
class RecordingSink(Sink):
    calls: list[tuple[str, int]] = field(default_factory=list)

    def log(self, pr, prev, status):
        self.calls.append(("log", pr))

    def notify(self, pr, status):
        self.calls.append(("notify", pr))

    def page(self, pr, status, *, reason):
        self.calls.append(("page", pr))

    def flip_ready(self, pr, status):
        self.calls.append(("flip_ready", pr))

    def spawn_fixer(self, pr, status):
        self.calls.append(("spawn_fixer", pr))

    def error(self, pr, exc):
        self.calls.append(("error", pr))


def test_poll_dispatches_then_dedups_same_state():
    sink = RecordingSink()
    last: dict[int, str] = {}
    feed = {7: status(TaskState.ADDRESSING)}

    poll_once([7], get_status=feed.get, last_states=last, sink=sink, auto=False)
    poll_once([7], get_status=feed.get, last_states=last, sink=sink, auto=False)

    # First pass dispatches (log + notify); second pass is a no-op (same state).
    assert sink.calls == [("log", 7), ("notify", 7)]
    assert last[7] == "addressing"


def test_poll_redispatches_on_transition():
    sink = RecordingSink()
    last: dict[int, str] = {}
    state_box = {7: status(TaskState.ADDRESSING)}

    poll_once([7], get_status=state_box.get, last_states=last, sink=sink, auto=True)
    state_box[7] = status(TaskState.READY)
    poll_once([7], get_status=state_box.get, last_states=last, sink=sink, auto=True)

    assert sink.calls == [
        ("log", 7),
        ("spawn_fixer", 7),  # ADDRESSING + auto
        ("log", 7),
        ("flip_ready", 7),  # READY
    ]


def test_poll_survives_status_error_and_keeps_going():
    # A transient failure on one PR must not crash the daemon — it logs the
    # error, skips that PR, and still dispatches the healthy ones.
    sink = RecordingSink()

    def flaky(pr):
        if pr == 1:
            raise RuntimeError("gh timed out")
        return status(TaskState.READY)

    poll_once([1, 2], get_status=flaky, last_states={}, sink=sink, auto=False)
    assert ("error", 1) in sink.calls
    assert ("flip_ready", 2) in sink.calls


def test_poll_handles_several_prs():
    sink = RecordingSink()
    feed = {1: status(TaskState.READY), 2: status(TaskState.BLOCKED, breaker="diff-trajectory")}
    poll_once([1, 2], get_status=feed.get, last_states={}, sink=sink, auto=True)
    kinds = {(k, p) for k, p in sink.calls}
    assert ("flip_ready", 1) in kinds
    assert ("page", 2) in kinds


# --- fixer prompt ---------------------------------------------------------


def test_run_is_bounded_and_dedups_across_passes():
    sink = RecordingSink()
    feed = {1: status(TaskState.VALIDATING)}  # a WAIT state
    run([1], sink=sink, auto=False, interval=0, get_status=feed.get, max_passes=3)
    # 3 passes, but only the first transition logs; WAIT does no dispatch.
    assert sink.calls == [("log", 1)]


def test_fixer_prompt_is_fresh_and_breaker_aware():
    p = build_fixer_prompt(42)
    assert "PR #42" in p
    assert "gh-task-status 42" in p
    assert "breaker" in p.lower() and "STOP" in p
    assert "handoff" in p.lower()
    assert "do not merge" in p.lower()
