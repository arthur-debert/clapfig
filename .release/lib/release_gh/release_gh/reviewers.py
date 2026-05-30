"""Reviewer adapters — the only place that knows reviewer-specific mechanics.

The state machine consumes the adapter interface (`required`, `detect`,
`open_threads`) and never branches on a reviewer's name. Adding a reviewer is
adding an adapter to `REGISTRY`; nothing downstream changes. This is what keeps
the core stable as the coding-agent landscape shifts.
"""

from __future__ import annotations

from .model import PullContext, ReviewLifecycle, Thread


class ReviewerAdapter:
    """Base adapter. Subclasses define `name`, `required`, `matches`, `detect`."""

    name: str = ""
    required: bool = False

    def matches(self, login: str) -> bool:
        raise NotImplementedError

    def detect(self, ctx: PullContext) -> ReviewLifecycle:
        raise NotImplementedError

    def authored_threads(self, ctx: PullContext) -> list[Thread]:
        """All threads (resolved or not) rooted in a comment by this reviewer."""
        return [t for t in ctx.threads if t.author and self.matches(t.author)]

    def open_threads(self, ctx: PullContext) -> list[Thread]:
        """Unresolved threads by this reviewer — the ones still needing action."""
        return [t for t in self.authored_threads(ctx) if not t.is_resolved]

    def _done_state(self, ctx: PullContext) -> ReviewLifecycle:
        return (
            ReviewLifecycle.DONE_COMMENTS
            if self.authored_threads(ctx)
            else ReviewLifecycle.DONE_CLEAN
        )


class CopilotAdapter(ReviewerAdapter):
    """Copilot posts a discrete review object on the PR head SHA.

    The head-SHA filter is load-bearing: a review against an earlier commit is
    stale and must not count as done for the current head. Copilot has no
    observable mid-review signal, so it goes REQUESTED -> DONE.
    """

    name = "copilot"
    required = True

    def matches(self, login: str) -> bool:
        return "copilot" in login.lower()

    def detect(self, ctx: PullContext) -> ReviewLifecycle:
        if any(self.matches(r.author) for r in ctx.reviews_on_head()):
            return self._done_state(ctx)
        if any(self.matches(login) for login in ctx.requested_logins):
            return ReviewLifecycle.REQUESTED
        return ReviewLifecycle.NOT_REQUESTED


class GeminiAdapter(ReviewerAdapter):
    """Gemini signals weakly and is best-effort.

    The app triggers automatically (no discrete request event); an eyes reaction
    from the bot means it is looking; a review or issue comment means it is done.
    It goes over quota silently, so the state machine treats a timed-out Gemini
    as skipped rather than blocking Ready — that timing decision lives in the
    state machine, not here.

    Crucially, **Gemini reviews a PR once and does not re-review pushes** — so a
    review on *any* commit of this PR counts as done, unlike Copilot's
    head-strict model. (The eyes reaction is not commit-scoped and lingers after
    the review, so a fixup that creates a new head would otherwise read as a
    fresh "in_progress" forever.) This per-reviewer difference is exactly what
    the adapter layer exists to hold.
    """

    name = "gemini"
    required = False

    def matches(self, login: str) -> bool:
        return "gemini" in login.lower()

    def detect(self, ctx: PullContext) -> ReviewLifecycle:
        # Any-head, not head-strict: Gemini won't review the new head again.
        if any(self.matches(r.author) for r in ctx.reviews):
            return self._done_state(ctx)
        if any(self.matches((c.get("user") or {}).get("login", "")) for c in ctx.issue_comments):
            return ReviewLifecycle.DONE_COMMENTS
        if self._is_looking(ctx):
            return ReviewLifecycle.IN_PROGRESS
        return ReviewLifecycle.NOT_REQUESTED

    def _is_looking(self, ctx: PullContext) -> bool:
        return any(
            r.get("content") == "eyes" and self.matches((r.get("user") or {}).get("login", ""))
            for r in ctx.reactions
        )


# The canonical, uniform reviewer set for every consumer. Defined once, here.
REGISTRY: list[ReviewerAdapter] = [CopilotAdapter(), GeminiAdapter()]


def required_reviewers() -> list[ReviewerAdapter]:
    return [r for r in REGISTRY if r.required]
