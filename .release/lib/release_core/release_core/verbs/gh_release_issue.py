"""File a bug against arthur-debert/release from inside a consumer repo.

Usage:
  gh-release-issue <component> <one-line-symptom>

Components (free-form, but standardize on these):
  copilot-review     — copilot-review.yml workflow misbehaves
  pr-review-loop     — gh-copilot-{on,off,wait} or related helpers
  rust-cli-release   — rust-cli reusable release workflow
  ruleset            — apply-ruleset / branch protection
  sweep-policy       — sweep-github-policy / templates
  install-token      — install-release-token / install-release-secrets
  other              — anything else

The script auto-collects the current repo, branch, and (when run inside a
PR branch) the PR number + most recent run of the relevant workflow, so
the issue body has reproduction context. The reporting agent should add
more detail to the issue after it's filed (logs, conjecture, etc.).

Why this exists: agents hitting an infra bug in a consumer repo can't
fix it in place — the fix lives in arthur-debert/release. Filing here
gives one inbox for cross-repo, cross-org infra bugs.

Exit codes:
  0  — issue filed (URL printed to stdout)
  64 — bad usage

Example:
  gh-release-issue copilot-review "workflow ran SUCCESS but requested_reviewers stayed empty"

Shell→Python migration (docs/proposals/shell-to-python.md): the gh-porcelain
context collection + heredoc body assembly moved into Python (no jq -q
extraction). Title, body (including the conditional PR/run lines), the
consumer-filed label, and the stdout URL are preserved byte-for-byte.
"""

from __future__ import annotations

import sys

from .. import gh, proc

USAGE = __doc__ or ""

RELEASE_REPO = "arthur-debert/release"

# component → the workflow whose most-recent run we link, when applicable.
_WORKFLOW_BY_COMPONENT = {
    "copilot-review": "copilot-review.yml",
    "rust-cli-release": "release.yml",
}


def _usage_block() -> str:
    """The pre-migration `show_help` text (everything up to the migration note)."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def _safe_out(cmd: list[str], default: str = "") -> str:
    """`proc.out` that swallows failure → default (mirrors bash `… || echo …`
    and `2>/dev/null`). Empty stdout also yields the default."""
    try:
        value = proc.out(cmd, check=True)
    except proc.ProcError:
        return default
    return value if value else default


def _safe_gh(fn, default: str = "") -> str:
    """Run a gh.* wrapper, swallowing GhError → default (mirrors bash
    `… || echo …`). Empty stdout also yields the default."""
    try:
        value = fn()
    except gh.GhError:
        return default
    return value if value else default


def collect_context() -> dict:
    """Auto-collected reproduction context: repo, branch, PR number/url, run url.

    Mirrors the bash gh/git probes exactly, including the `(unknown)` sentinels
    and the branch-scoped run lookup. Each probe degrades gracefully.
    """
    repo = _safe_gh(
        lambda: gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner"),
        "(unknown)",
    )
    branch = _safe_out(["git", "branch", "--show-current"], "(unknown)")

    pr_number = ""
    pr_url = ""
    if repo != "(unknown)" and branch != "(unknown)":
        pr_number = _safe_gh(
            lambda: gh.pr_list(head=branch, json_fields=["number"], q=".[0].number // empty")
        )
        if pr_number:
            pr_url = f"https://github.com/{repo}/pull/{pr_number}"

    return {"repo": repo, "branch": branch, "pr_number": pr_number, "pr_url": pr_url}


def lookup_run_url(component: str, branch: str) -> str:
    """Most-recent run URL of the workflow mapped to `component`, scoped to
    `branch` when known. Empty for components with no mapped workflow."""
    workflow = _WORKFLOW_BY_COMPONENT.get(component)
    if not workflow:
        return ""
    run_branch = branch if branch != "(unknown)" else None
    return _safe_gh(lambda: _run_list_url(workflow, run_branch))


def _run_list_url(workflow: str, branch: str | None) -> str:
    """`gh run list --workflow <wf> [--branch <b>] --limit 1 --json url -q …`
    → the run URL (empty when none). Byte-identical argv to the former
    `gh run list` call, branch flag omitted when unknown."""
    result = gh.run_list(
        workflow=workflow,
        branch=branch,
        limit=1,
        json_fields=["url"],
        q=".[0].url // empty",
    )
    if result.returncode != 0:
        raise gh.GhError(result.stderr.strip())
    return result.stdout.strip()


def build_title(component: str, symptom: str) -> str:
    return f"[{component}] {symptom}"


def build_body(component: str, symptom: str, ctx: dict, run_url: str) -> str:
    """The issue body. The PR/run lines appear ONLY when populated, matching the
    bash `${pr_url:+**PR:** $pr_url}` conditional-expansion (an empty value
    collapses the whole line, leaving no blank line behind)."""
    lines = [
        f"**Component:** {component}",
        f"**Reported from:** {ctx['repo']}",
        f"**Branch:** {ctx['branch']}",
    ]
    if ctx["pr_url"]:
        lines.append(f"**PR:** {ctx['pr_url']}")
    if run_url:
        lines.append(f"**Workflow run:** {run_url}")
    lines += [
        "",
        f"**Symptom:** {symptom}",
        "",
        "---",
        "",
        "_Filed via `gh-release-issue`. The reporting agent should follow up with",
        "logs, suspected cause, and any reproduction steps that aren't obvious",
        "from the links above._",
    ]
    return "\n".join(lines)


def _create_issue(title: str, body: str) -> str:
    """`gh issue create … --label consumer-filed` → the issue URL on stdout.

    The `consumer-filed` label is the marker the fleet inbox (release-inbox)
    filters on. Porcelain (not REST) so it stays the single gh chokepoint and
    matches the offline test stub.
    """
    return gh.issue_create(repo=RELEASE_REPO, title=title, body=body, label="consumer-filed")


def main(argv: list[str]) -> int:
    # `-h`/`--help` (or no args) before arity, matching the bash case/`$1` checks.
    first = argv[0] if argv else ""
    if first in ("-h", "--help"):
        print(_usage_block())
        return 0
    if first == "":
        print(_usage_block(), file=sys.stderr)
        return 64

    if len(argv) < 2:
        print("error: need <component> <symptom>", file=sys.stderr)
        print(_usage_block(), file=sys.stderr)
        return 64

    component = argv[0]
    # Everything after the component is the symptom, so a multi-word symptom is
    # captured whole (mirrors the bash `shift; symptom="$*"` — space-joined).
    symptom = " ".join(argv[1:])

    ctx = collect_context()
    run_url = lookup_run_url(component, ctx["branch"])

    title = build_title(component, symptom)
    body = build_body(component, symptom, ctx, run_url)

    url = _create_issue(title, body)
    print(url)
    return 0
