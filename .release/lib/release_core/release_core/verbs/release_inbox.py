"""release-inbox — triage view over consumer-filed issues on this repo.

The self-improving feedback loop (release#348) routes fleet friction into ONE
inbox: the arthur-debert/release issue tracker. Producers — the escalation
contract (`gh-release-issue` / the `release-issue-relay` skill) and the Phase D
CI sweep — file issues here tagged `consumer-filed`, titled `[<component>]
<symptom>`, with a `Reported from: <repo>` line in the body.

This accessor reads that inbox and renders a triage-ready digest for the
Phase C batch-processing run: open issues grouped by component, clusters
sorted by recurrence (comment count is the "also hit here" signal that the
relay skill appends on a duplicate), each issue showing its source repo and
age. Read-only — it files and mutates nothing.

Usage:
  release-inbox                  # human-readable digest (default)
  release-inbox --json           # machine-readable clusters (Phase C consumes this)
  release-inbox --label <name>   # override the marker label (default: consumer-filed)
  release-inbox --repo <o/r>     # override the inbox repo (default: arthur-debert/release)

Exit codes:
  0  — rendered (including when the inbox is empty)
  2  — dependency or auth error
  64 — bad usage

Shell→Python migration: the jq clustering
pass moved into Python (gh.rest → parsed dicts, no jq). The --json shape and the
human digest are preserved; release#348 Phase C consumes the --json clusters.
"""

from __future__ import annotations

import datetime
import json
import re
import sys

from .. import gh

USAGE = __doc__ or ""

_COMPONENT_RE = re.compile(r"^\[([^\]]+)\]")
_SYMPTOM_RE = re.compile(r"^\[[^\]]+\]\s*")
# `Reported from:` optionally followed by markdown bold `**`, then the repo up to
# a newline or `*`. Mirrors the jq capture "Reported from:\**\s*(?<r>[^\n*]+)".
_SOURCE_RE = re.compile(r"Reported from:\**\s*([^\n*]+)")


def _usage_block() -> str:
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def _component(title: str) -> str:
    m = _COMPONENT_RE.match(title)
    return m.group(1) if m else "other"


def _symptom(title: str) -> str:
    return _SYMPTOM_RE.sub("", title)


def _source_repo(body: str | None) -> str:
    m = _SOURCE_RE.search(body or "")
    return m.group(1).strip() if m else "unknown"


def _age_days(created_at: str, now: datetime.datetime) -> int:
    # createdAt is ISO-8601 with a trailing Z; floor((now - created) / 1 day).
    created = datetime.datetime.fromisoformat(created_at.replace("Z", "+00:00"))
    return int((now - created).total_seconds() // 86400)


def cluster(issues: list[dict], now: datetime.datetime | None = None) -> list[dict]:
    """Normalize → group by component → sort. Pure; the BATS/pytest oracle.

    Mirrors the bash jq pass exactly:
      - per-issue: component (title prefix), repo (body), symptom, comments, age
      - group_by(component); recurrence = sum(comments) + issue_count
      - issues within a cluster sorted by (-comments, age_days)
      - clusters sorted by -recurrence
    """
    if now is None:
        now = datetime.datetime.now(datetime.UTC)

    normalized = [
        {
            "number": issue["number"],
            "url": issue["url"],
            "component": _component(issue["title"]),
            "repo": _source_repo(issue.get("body")),
            "symptom": _symptom(issue["title"]),
            "comments": len(issue.get("comments") or []),
            "age_days": _age_days(issue["createdAt"], now),
        }
        for issue in issues
    ]

    # group_by(.component): jq groups by sorted key and preserves input order
    # within a group. Replicate: stable grouping keyed on first-seen component
    # order would diverge from jq (which sorts keys), so sort the *component
    # keys* the way jq's group_by does — lexicographically — before the final
    # recurrence sort (which is stable, so it only matters for ties).
    by_component: dict[str, list[dict]] = {}
    for item in normalized:
        by_component.setdefault(item["component"], []).append(item)

    clusters = []
    for component in sorted(by_component):
        members = by_component[component]
        clusters.append(
            {
                "component": component,
                "issue_count": len(members),
                "recurrence": sum(m["comments"] for m in members) + len(members),
                # sort_by(-.comments, .age_days): most-commented first, then oldest-age tiebreak.
                "issues": sorted(members, key=lambda m: (-m["comments"], m["age_days"])),
            }
        )

    # sort_by(-.recurrence): highest recurrence first. Python's sort is stable, so
    # equal-recurrence clusters keep their lexicographic component order (matching
    # jq, whose sort_by is also stable over the group_by'd key order).
    clusters.sort(key=lambda c: -c["recurrence"])
    return clusters


def render_human(clusters: list[dict], label: str) -> str:
    if not clusters:
        return f"Fleet inbox: no open `{label}` issues — clear."
    total = sum(c["issue_count"] for c in clusters)
    lines = [
        f"Fleet inbox — {total} open issue(s) across {len(clusters)} component(s), by recurrence:",
        "",
    ]
    for c in clusters:
        lines.append(
            f"▌ {c['component']}   {c['issue_count']} issue(s) · recurrence {c['recurrence']}"
        )
        for issue in c["issues"]:
            lines.append(f"    #{issue['number']}  {issue['repo']}  —  {issue['symptom']}")
            lines.append(
                f"        {issue['comments']} comment(s) · "
                f"{issue['age_days']}d old · {issue['url']}"
            )
        lines.append("")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    repo = "arthur-debert/release"
    label = "consumer-filed"
    out = "human"

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--json":
            out = "json"
        elif arg == "--label":
            if i + 1 >= len(argv):
                print("release-inbox: --label needs a value", file=sys.stderr)
                return 64
            i += 1
            label = argv[i]
        elif arg == "--repo":
            if i + 1 >= len(argv):
                print("release-inbox: --repo needs a value", file=sys.stderr)
                return 64
            i += 1
            repo = argv[i]
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        else:
            print(f"release-inbox: unknown argument '{arg}'", file=sys.stderr)
            print(_usage_block(), file=sys.stderr)
            return 64
        i += 1

    try:
        raw = gh.issue_list(
            repo,
            state="open",
            label=label,
            limit=200,
            json_fields=["number", "title", "body", "createdAt", "url", "comments"],
        )
    except gh.GhError:
        print(
            f"release-inbox: could not read issues from {repo} (auth or network?)",
            file=sys.stderr,
        )
        return 2

    clusters = cluster(raw)

    if out == "json":
        print(json.dumps(clusters, indent=2))
        return 0

    print(render_human(clusters, label))
    return 0
