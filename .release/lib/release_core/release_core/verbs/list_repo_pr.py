"""list-repo-pr — for each managed repo, list open PRs with status info.

Shows: PR number, draft/ready, CI status, mergeable, review comments,
unresolved threads, title, and PR link. Designed as a daily dashboard.
The PR link is colored green when the PR is merge-ready (CI passes,
mergeable, no unresolved threads).

Usage:
  list-repo-pr                 # all managed repos
  list-repo-pr --only-present  # skip repos missing locally
  list-repo-pr --owner lex-fmt # filter by owner prefix

Shell→Python migration (docs/proposals/shell-to-python.md): the per-PR jq field
extraction moved into Python over the parsed GraphQL dict (gh.graphql, no jq).
The human table — column widths, ANSI colors, the merge-ready green URL — is
preserved byte-for-byte.
"""

from __future__ import annotations

import os
import sys

from .. import gh

USAGE = __doc__ or ""

# repo|local-path — the canonical "managed by arthur-debert/release" set. NOT
# auto-discovered (the ruleset signal includes unrelated repos). Kept verbatim
# from the bash so the contract is byte-identical.
REPOS = [
    ("arthur-debert/release", "{home}/h/release"),
    ("arthur-debert/phos-app", "{home}/h/phos/phos-app"),
    ("arthur-debert/phos-core", "{home}/h/phos/phos-core"),
    ("arthur-debert/burgertocow", "{home}/h/burgertocow"),
    ("arthur-debert/clapfig", "{home}/h/clapfig"),
    ("arthur-debert/dodot", "{home}/h/dodot"),
    ("arthur-debert/homebrew-tools", "{home}/h/homebrew-tools"),
    ("arthur-debert/padz", "{home}/h/padz"),
    ("arthur-debert/rustloc", "{home}/h/rustloc"),
    ("arthur-debert/simple-gal", "{home}/h/simple-gal/simple-gal"),
    ("arthur-debert/simple-gal-action", "{home}/h/simple-gal/simple-gal-action"),
    ("arthur-debert/simple-gal-ui", "{home}/h/simple-gal/simple-gal-ui"),
    ("arthur-debert/standout", "{home}/h/standout"),
    ("lex-fmt/comms", "{home}/h/lex-fmt/comms"),
    ("lex-fmt/lex", "{home}/h/lex-fmt/lex"),
    ("lex-fmt/lexed", "{home}/h/lex-fmt/lexed"),
    ("lex-fmt/nvim", "{home}/h/lex-fmt/nvim"),
    ("lex-fmt/tree-sitter-lex", "{home}/h/lex-fmt/tree-sitter-lex"),
    ("lex-fmt/vscode", "{home}/h/lex-fmt/vscode"),
    ("lex-fmt/zed-lex", "{home}/h/lex-fmt/zed-lex"),
]

QUERY = """
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 50, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        url
        isDraft
        mergeable
        author { login }
        headRefName
        reviewThreads(first: 100) {
          totalCount
          nodes { isResolved }
        }
        reviews(first: 100) {
          totalCount
          nodes { comments { totalCount } }
        }
        comments { totalCount }
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup { state }
            }
          }
        }
      }
    }
  }
}"""

R = "\033[0m"
RED = "\033[31m"
GRN = "\033[32m"
YLW = "\033[33m"


def _cell(width: int, clr: str, text: str) -> str:
    return f"{clr}{text:<{width}}{R}"


def _repos() -> list[tuple[str, str]]:
    home = os.path.expanduser("~")
    return [(repo, path.format(home=home)) for repo, path in REPOS]


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch + render mirrors the bash
    only_present = False
    owner_filter = ""

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--only-present":
            only_present = True
        elif arg == "--owner":
            i += 1
            owner_filter = argv[i] if i < len(argv) else ""
        elif arg in ("-h", "--help"):
            print(USAGE.strip().split("\n\nShell→Python")[0])
            return 0
        else:
            print(f"unknown arg: {arg}", file=sys.stderr)
            return 64
        i += 1

    first = True
    for repo, path in _repos():
        owner, _, name = repo.partition("/")
        if owner_filter and owner != owner_filter:
            continue
        if only_present and not os.path.isdir(path):
            continue

        try:
            data = gh.graphql(QUERY, owner=owner, name=name)
        except gh.GhError as exc:
            if not first:
                print()
            first = False
            print(f"=== {repo} ===")
            print(f"  (API error: {exc})")
            continue

        nodes = data["repository"]["pullRequests"]["nodes"]
        if len(nodes) == 0:
            continue

        if not first:
            print()
        first = False
        print(f"=== {repo} ({len(nodes)} open) ===")
        _print_header()
        for pr in nodes:
            _print_row(pr)

    if first:
        print("(no open PRs across managed repos)")
    return 0


def _print_header() -> None:
    fmt = "  {:<6} {:<7} {:<8} {:<10} {:<8} {:<10} {:<52} {}"
    print(fmt.format("#", "status", "CI", "merge", "cmnts", "unresolvd", "title", "url"))
    print(
        fmt.format(
            "------",
            "-------",
            "--------",
            "----------",
            "--------",
            "----------",
            "-" * 30,
            "---",
        )
    )


def _print_row(pr: dict) -> None:
    num = pr["number"]
    url = pr["url"]
    title = pr["title"][:50]
    is_draft = pr["isDraft"]
    mergeable = pr["mergeable"]

    commit_nodes = pr["commits"]["nodes"]
    rollup = commit_nodes[0]["commit"]["statusCheckRollup"] if commit_nodes else None
    ci_state = rollup["state"] if rollup else "null"

    threads_unresolved = sum(1 for n in pr["reviewThreads"]["nodes"] if n["isResolved"] is False)
    inline_comments = sum(r["comments"]["totalCount"] for r in pr["reviews"]["nodes"])
    pr_comments = pr["comments"]["totalCount"]
    total_comments = inline_comments + pr_comments

    if is_draft:
        st_text, st_clr = "draft", YLW
    else:
        st_text, st_clr = "ready", GRN

    ci_map = {
        "SUCCESS": ("+ pass", GRN),
        "PENDING": ("~ pend", YLW),
        "FAILURE": ("x fail", RED),
        "ERROR": ("x err", RED),
    }
    ci_text, ci_clr = ci_map.get(ci_state, ("-", ""))

    mg_map = {
        "MERGEABLE": ("yes", GRN),
        "CONFLICTING": ("conflict", RED),
    }
    mg_text, mg_clr = mg_map.get(mergeable, ("?", YLW))

    cm_text = str(total_comments) if total_comments > 0 else "-"

    if threads_unresolved > 0:
        ur_text, ur_clr = str(threads_unresolved), YLW
    else:
        ur_text, ur_clr = "-", ""

    url_clr = ""
    if ci_state == "SUCCESS" and mergeable == "MERGEABLE" and threads_unresolved == 0:
        url_clr = GRN

    line = f"  {('#' + str(num)):<6} "
    line += _cell(7, st_clr, st_text) + " "
    line += _cell(8, ci_clr, ci_text) + " "
    line += _cell(10, mg_clr, mg_text) + " "
    line += _cell(8, "", cm_text) + " "
    line += _cell(10, ur_clr, ur_text)
    line += f" {title:<52} "
    line += f"{url_clr}{url}{R}"
    print(line)
