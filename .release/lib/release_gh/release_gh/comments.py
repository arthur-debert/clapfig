"""List / reply / resolve PR review threads, over the `gh` boundary.

Reply and resolve are the two write actions the review loop needs after
triaging a comment (fix-and-push or push-back-with-rationale). Listing returns
the open threads with the handles those actions require.
"""

from __future__ import annotations

from . import fetch, ghapi
from .model import Thread

_RESOLVE = """
mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread { isResolved }
  }
}
"""


def open_threads(pr: int) -> list[Thread]:
    """All unresolved review threads on the PR (each carries path/line/ids)."""
    return fetch.gather(pr).open_threads()


def reply(pr: int, comment_id: int, body: str) -> None:
    """Reply to a review comment, keeping the thread (does not resolve it)."""
    owner, name = ghapi.repo_slug()
    ghapi.rest(
        f"repos/{owner}/{name}/pulls/{pr}/comments/{comment_id}/replies",
        method="POST",
        fields={"body": body},
    )


def resolve(thread_id: str) -> None:
    """Mark a review thread resolved via the GraphQL mutation."""
    ghapi.graphql(_RESOLVE, threadId=thread_id)
