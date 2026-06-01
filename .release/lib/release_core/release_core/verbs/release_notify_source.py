"""release-notify-source — close the feedback loop on a consumer-filed issue.

When the fleet-triage run (release#348 §5.2, the `release-fleet-triage` skill)
ships a fix for an issue that consumers escalated via `gh-release-issue` /
the `release-issue-relay` skill, the consumers who reported it need to know:
the fix lives upstream, bump the `@vN` pin (or re-sync) and re-run.

This reads ONE release issue, extracts every source PR it points at (the
`**PR:** <url>` line in the body plus the `- PR: <url>` lines the relay skill
appends on each "Also hit on" duplicate), and posts a consistent notification
comment on each. The message shape is owned here, not hand-written per repo
(per docs/references/fleet-telemetry-via-issues.md §2).

Outward-facing fan-out across consumer repos, so it is DRY-RUN BY DEFAULT:
it prints the plan and the exact comment body. Pass --post to actually comment.

Usage:
  release-notify-source <issue-#> --fix "<text>" [--post] [--close]

  <issue-#>      a release issue number (on arthur-debert/release)
  --fix <text>   one line describing the shipped fix (e.g. "release#371,
                 v2 advanced to 9588a8f") — embedded in the notification
  --post         actually post the comments (default: dry-run, prints only)
  --close        after posting, close the release issue with a back-reference
                 (ignored in dry-run)
  --repo <o/r>   override the inbox repo (default: arthur-debert/release)

Exit codes:
  0  — done (dry-run or posted)
  2  — dependency/auth error, or no such issue
  3  — the issue points at no source PR (nothing to notify; reported loud)
  64 — bad usage

Shell→Python migration (docs/proposals/shell-to-python.md): the jq blob
flattening + grep PR/repo extraction moved into Python over the parsed issue
dict (gh porcelain, no jq). The dry-run/--post/--close gating, the notification
body, and every stdout/stderr line are preserved byte-for-byte.
"""

from __future__ import annotations

import json
import re
import sys

from .. import gh

USAGE = __doc__ or ""

_PR_URL_RE = re.compile(r"https://github\.com/[^/]+/[^/]+/pull/[0-9]+")
# Mirrors the bash two-stage grep: a "Reported from:" / "Also hit on" prefix
# (optionally **bold**) followed by an owner/repo slug, then the slug is pulled
# back out on its own.
_REPORTED_RE = re.compile(
    r"(?:Reported from:|Also hit on) ?\*{0,2} ?([A-Za-z0-9._-]+/[A-Za-z0-9._-]+)"
)


def _usage_block() -> str:
    """The pre-migration `show_help` text (everything up to the migration note)."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def extract_pr_urls(blob: str) -> list[str]:
    """Distinct consumer PR URLs referenced anywhere in the blob, sorted.

    PR URLs are unambiguous comment targets (each encodes its own repo), so we
    don't pair repo↔PR by hand. Mirrors `grep -oE … | sort -u`.
    """
    return sorted(set(_PR_URL_RE.findall(blob)))


def extract_reported_repos(blob: str) -> list[str]:
    """`Reported from:` / `Also hit on` repos, sorted unique — for the no-PR
    case where there's no comment target and the operator must follow up by
    hand. Mirrors the bash two-stage grep + `sort -u`."""
    return sorted(set(_REPORTED_RE.findall(blob)))


def build_blob(meta: dict) -> str:
    """Flatten body + every comment body into one text blob.

    Mirrors `jq -r '.body, (.comments[]?.body // "")'`: the body first, then
    one line per comment body (null → empty string), newline-joined.
    """
    parts: list[str] = [meta.get("body") or ""]
    for comment in meta.get("comments") or []:
        parts.append(comment.get("body") or "")
    return "\n".join(parts)


def notification_body(issue: int, issue_url: str, issue_title: str, fix: str) -> str:
    """The notification comment posted on each source PR. Byte-identical to the
    bash heredoc (owned here, not hand-written per repo)."""
    return (
        f"✅ **Upstream fix shipped.** This resolves the infrastructure issue "
        f"escalated to [arthur-debert/release#{issue}]({issue_url}) — "
        f"_{issue_title}_.\n"
        f"\n"
        f"**Fix:** {fix}\n"
        f"\n"
        f"To pick it up: bump your `arthur-debert/release` pin to the advanced "
        f"major (e.g. `@v2`) or re-run `release-sync`, then re-run the check "
        f"that originally failed. Nothing to change in this repo.\n"
        f"\n"
        f"_Automated close-the-loop from the release fleet-triage run "
        f"(release#348 §5.2)._"
    )


def _view_issue(issue: str, repo: str) -> dict:
    """`gh issue view <issue> --repo <repo> --json …` → parsed dict.

    Porcelain (not REST) so the offline BATS `gh` stub — which special-cases
    `gh issue view` — keeps working. Raises gh.GhError on failure (→ exit 2).
    """
    result = gh.issue_view(
        issue, repo=repo, json_fields=["number", "title", "url", "body", "comments"]
    )
    if result.returncode != 0:
        raise gh.GhError(result.stderr.strip())
    return json.loads(result.stdout)


def _comment_pr(url: str, body: str) -> bool:
    """`gh pr comment <url> --body <body>`. True on success, False on failure
    (the bash swallows gh's own output and only reports success/FAILED)."""
    return gh.pr_comment(url, body=body).returncode == 0


def _close_issue(issue: str, repo: str, comment: str) -> bool:
    """`gh issue close <issue> --repo <repo> --comment <comment>`."""
    return gh.issue_close(issue, repo=repo, comment=comment).returncode == 0


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash
    repo = "arthur-debert/release"
    issue = ""
    fix = ""
    post = False
    close = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--fix":
            if i + 1 >= len(argv):
                print("release-notify-source: --fix needs a value", file=sys.stderr)
                return 64
            i += 1
            fix = argv[i]
        elif arg == "--repo":
            if i + 1 >= len(argv):
                print("release-notify-source: --repo needs a value", file=sys.stderr)
                return 64
            i += 1
            repo = argv[i]
        elif arg == "--post":
            post = True
        elif arg == "--close":
            close = True
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        elif arg.startswith("-"):
            print(f"release-notify-source: unknown argument '{arg}'", file=sys.stderr)
            print(_usage_block(), file=sys.stderr)
            return 64
        else:
            if issue:
                print(
                    "release-notify-source: too many positional args",
                    file=sys.stderr,
                )
                return 64
            issue = arg
        i += 1

    if not issue:
        print(
            "release-notify-source: need a release issue number",
            file=sys.stderr,
        )
        print(_usage_block(), file=sys.stderr)
        return 64
    if not fix:
        print(
            "release-notify-source: --fix is required (describe the shipped fix)",
            file=sys.stderr,
        )
        return 64
    if not issue.isdigit():
        print(
            f"release-notify-source: issue must be a number, got '{issue}'",
            file=sys.stderr,
        )
        return 64

    try:
        meta = _view_issue(issue, repo)
    except gh.GhError:
        print(
            f"release-notify-source: could not read {repo}#{issue} "
            "(no such issue, auth, or network?)",
            file=sys.stderr,
        )
        return 2

    issue_url = meta["url"]
    issue_title = meta["title"]

    blob = build_blob(meta)
    pr_urls = extract_pr_urls(blob)
    reported_repos = extract_reported_repos(blob)

    notify_body = notification_body(int(issue), issue_url, issue_title, fix)

    print(f"release issue:  {repo}#{issue} — {issue_title}")
    print(f"source PRs:     {len(pr_urls)}")
    if len(pr_urls) == 0:
        print(
            f"release-notify-source: {repo}#{issue} points at no source PR — "
            "nothing to comment on.",
            file=sys.stderr,
        )
        if reported_repos:
            print(
                f"  Reported from (notify by hand): {' '.join(reported_repos)}",
                file=sys.stderr,
            )
        return 3

    if not post:
        print()
        print("DRY-RUN — would comment on each PR below (pass --post to send):")
        for u in pr_urls:
            print(f"  • {u}")
        print()
        print("--- comment body ------------------------------------------------------")
        print(notify_body)
        print("-----------------------------------------------------------------------")
        if close:
            print(f"(--close) would then close {repo}#{issue} with a back-reference.")
        return 0

    rc = 0
    for u in pr_urls:
        if _comment_pr(u, notify_body):
            print(f"notified: {u}")
        else:
            print(f"FAILED to comment on {u}", file=sys.stderr)
            rc = 1

    if reported_repos:
        print(
            "note — reported-from repos seen (verify each got a PR comment above): "
            f"{' '.join(reported_repos)}"
        )

    if close and rc == 0:
        comment = f"Closed by fleet-triage: {fix}. Notified source PR(s): {' '.join(pr_urls)}."
        if _close_issue(issue, repo, comment):
            print(f"closed: {repo}#{issue}")

    return rc
