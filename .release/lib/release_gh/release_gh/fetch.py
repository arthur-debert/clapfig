"""Gather all raw GitHub state for one PR into a `PullContext`.

The only module that calls `ghapi` on read paths. The raw-JSON -> model parsing
is split out (`context_from_raw`) so tests can build a context from recorded
fixtures without the network, exercising the exact code `gather()` runs live.
"""

from __future__ import annotations

from . import ghapi
from .model import PullContext, Review, ReviewComment, Thread

# `comments(first: 100)` is deliberately un-paginated: the engine gates on a
# thread's existence + `isResolved` + its root author, all of which live in the
# thread node and its first comment, so truncating a >100-comment thread's tail
# can't flip a gating decision. Thread COUNT is the real risk (a missed thread
# is a missed unresolved blocker), so reviewThreads IS paginated via the cursor.
_THREADS_QUERY = """
query($owner: String!, $name: String!, $pr: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $pr) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          isResolved
          comments(first: 100) {
            nodes { databaseId path line originalLine body author { login } }
          }
        }
      }
    }
  }
}
"""


def _all_review_threads(owner: str, name: str, pr: int) -> list[dict]:
    """Every review-thread node for the PR, following the cursor to the end.

    Without pagination a PR with >100 threads would silently truncate, and a
    dropped unresolved thread reads as READY when it isn't.
    """
    nodes: list[dict] = []
    cursor: str | None = None
    while True:
        data = ghapi.graphql(_THREADS_QUERY, owner=owner, name=name, pr=pr, cursor=cursor)
        conn = data["repository"]["pullRequest"]["reviewThreads"]
        nodes.extend(conn["nodes"])
        page = conn["pageInfo"]
        if not page["hasNextPage"]:
            return nodes
        cursor = page["endCursor"]


def gather(pr: int) -> PullContext:
    """Fetch every raw input the engine needs for `pr`, live, via `gh`."""
    owner, name = ghapi.repo_slug()
    base = f"repos/{owner}/{name}"
    meta = ghapi.pr_meta(pr)
    thread_nodes = _all_review_threads(owner, name, pr)
    return context_from_raw(
        meta=meta,
        reviews_json=ghapi.rest(f"{base}/pulls/{pr}/reviews", paginate=True) or [],
        thread_nodes=thread_nodes,
        reactions=ghapi.rest(f"{base}/issues/{pr}/reactions", paginate=True) or [],
        issue_comments=ghapi.rest(f"{base}/issues/{pr}/comments", paginate=True) or [],
        review_comments=ghapi.rest(f"{base}/pulls/{pr}/comments", paginate=True) or [],
    )


def context_from_raw(
    *,
    meta: dict,
    reviews_json: list[dict],
    thread_nodes: list[dict],
    reactions: list[dict],
    issue_comments: list[dict],
    review_comments: list[dict] | None = None,
) -> PullContext:
    """Pure: assemble a `PullContext` from raw gh payloads. No network."""
    return PullContext(
        number=meta["number"],
        head_sha=meta["headRefOid"],
        is_draft=bool(meta.get("isDraft")),
        base_ref=meta.get("baseRefName"),
        mergeable=meta.get("mergeable"),
        merge_state=meta.get("mergeStateStatus"),
        reviews=[_review(r) for r in reviews_json],
        threads=[_thread(n) for n in thread_nodes],
        reactions=reactions,
        issue_comments=issue_comments,
        requested_logins=_requested_logins(meta.get("reviewRequests") or []),
        checks=meta.get("statusCheckRollup") or [],
        review_comments=review_comments or [],
    )


def _review(raw: dict) -> Review:
    return Review(
        review_id=raw["id"],
        author=(raw.get("user") or {}).get("login", ""),
        state=raw.get("state", ""),
        commit_id=raw.get("commit_id", ""),
        body=raw.get("body") or "",
    )


def _thread(node: dict) -> Thread:
    comments = tuple(
        ReviewComment(
            comment_id=c["databaseId"],
            path=c.get("path") or "",
            line=c.get("line") or c.get("originalLine"),
            body=c.get("body") or "",
            author=(c.get("author") or {}).get("login", ""),
        )
        for c in node["comments"]["nodes"]
    )
    return Thread(thread_id=node["id"], is_resolved=node["isResolved"], comments=comments)


def _requested_logins(review_requests: list[dict]) -> list[str]:
    # User/Bot requests carry `login`; team requests carry `name`/`slug`.
    out = [(rr.get("login") or rr.get("name") or rr.get("slug") or "") for rr in review_requests]
    return [x for x in out if x]
