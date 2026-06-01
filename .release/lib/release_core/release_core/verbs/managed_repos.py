"""managed-repos — accessor for the portfolio fleet manifest.

Reads managed-repos.yaml (the ONLY source of truth — no discovery) and
maps each repo to its on-disk location with ZERO logic: the manifest
states each repo's `path`, and a repo resolves to `$REPOS_ROOT/<path>`.
No probing, no single-vs-multi heuristics, no org guessing.

$REPOS_ROOT defaults to ~/h (a dev machine). Point it at an empty dir
and use --clone for a fresh, self-contained fleet checkout.

Usage:
  managed-repos [--list]             # owner/name, one per line (default)
  managed-repos --paths              # owner/name <TAB> abspath <TAB> found|missing
  managed-repos --clone [--refresh]  # clone missing repos into their paths
                                     # (--refresh: also fetch+reset existing ones)

Any mode accepts trailing owner/name args to restrict to that subset:
  managed-repos --clone arthur-debert/padz lex-fmt/lex

Exit codes:
  0  — ok
  1  — a clone/refresh failed
  2  — manifest or dependency error
  64 — bad usage

Shell→Python migration (docs/proposals/shell-to-python.md): the logic moved to
this verb; bin/managed-repos is a thin shim. Stdout (the <TAB>-joined --paths /
--list lines) is preserved byte-for-byte — release-verify-fleet and
audit-portfolio parse it with `IFS=$'\t' read`.
"""

from __future__ import annotations

import os
import shutil
import sys

from .. import gh, proc, yamlio

USAGE = __doc__ or ""


def _help() -> None:
    # Mirror the bash `show_help`: print the usage block (lines 2..first blank
    # of the original header). We render the docstring's leading section, which
    # is the same content.
    print(_usage_block())


def _usage_block() -> str:
    """The help body — the docstring up to (but not including) the migration note."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def _usage_error(msg: str) -> int:
    print(msg, file=sys.stderr)
    print(_usage_block(), file=sys.stderr)
    return 64


def _manifest_path() -> str:
    """Resolve the manifest path, preserving the bash precedence:

    1. MANAGED_REPOS_MANIFEST — explicit override (tests point it at a fixture).
    2. <script_dir>/../managed-repos.yaml — the default the bash used, where
       script_dir is bin/. The shim exports MANAGED_REPOS_SCRIPT_DIR (its own
       realpath'd bin/ dir) so this resolves identically regardless of cwd.
    3. managed-repos.yaml in the cwd — last-ditch fallback if invoked outside
       the shim (the verb is release-only, so this is the repo root in practice).
    """
    override = os.environ.get("MANAGED_REPOS_MANIFEST")
    if override:
        return override
    script_dir = os.environ.get("MANAGED_REPOS_SCRIPT_DIR")
    if script_dir:
        return os.path.normpath(os.path.join(script_dir, "..", "managed-repos.yaml"))
    return "managed-repos.yaml"


def _pairs(manifest: str, filter_set: list[str]) -> list[tuple[str, str]]:
    """Every active entry as (repo, path), in manifest declaration order.

    `.projects` is a mapping of project-name → list of {repo, path}; flatten it.
    An optional owner/name filter restricts the set (order: manifest order,
    matching the bash which streams the manifest and tests each row)."""
    data = yamlio.load(manifest) or {}
    projects = data.get("projects") or {}
    pairs: list[tuple[str, str]] = []
    for entries in projects.values():
        for entry in entries or []:
            pairs.append((entry["repo"], entry["path"]))
    if filter_set:
        wanted = set(filter_set)
        pairs = [(r, p) for (r, p) in pairs if r in wanted]
    return pairs


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash modes
    mode = "list"
    refresh = False
    filter_set: list[str] = []

    for arg in argv:
        if arg == "--list":
            mode = "list"
        elif arg == "--paths":
            mode = "paths"
        elif arg == "--clone":
            mode = "clone"
        elif arg == "--refresh":
            refresh = True
        elif arg in ("-h", "--help"):
            _help()
            return 0
        elif arg.startswith("-"):
            return _usage_error(f"managed-repos: unknown arg: {arg}")
        else:
            filter_set.append(arg)

    manifest = _manifest_path()
    root = os.environ.get("REPOS_ROOT") or os.path.join(os.path.expanduser("~"), "h")

    if not os.path.isfile(manifest):
        print(f"managed-repos: manifest not found: {manifest}", file=sys.stderr)
        return 2
    if shutil.which("yq") is None:
        print("managed-repos: yq required (mikefarah/yq v4)", file=sys.stderr)
        return 2

    pairs = _pairs(manifest, filter_set)

    if mode == "list":
        for repo, _path in pairs:
            print(repo)
        return 0

    if mode == "paths":
        for repo, path in pairs:
            abspath = os.path.join(root, path)
            found = "found" if os.path.isdir(os.path.join(abspath, ".git")) else "missing"
            print(f"{repo}\t{abspath}\t{found}")
        return 0

    # mode == "clone"
    if shutil.which("gh") is None:
        print("managed-repos: gh required for --clone", file=sys.stderr)
        return 2
    return _clone(pairs, root, refresh)


def _clone(pairs: list[tuple[str, str]], root: str, refresh: bool) -> int:
    rc = 0
    for repo, path in pairs:
        abspath = os.path.join(root, path)
        if os.path.isdir(os.path.join(abspath, ".git")):
            if refresh:
                print(f"→ {repo}: refreshing {abspath}", file=sys.stderr)
                if not _refresh_one(repo, abspath):
                    print(f"→ {repo}: refresh FAILED", file=sys.stderr)
                    rc = 1
            else:
                print(f"→ {repo}: exists, skipping ({abspath})", file=sys.stderr)
        else:
            print(f"→ {repo}: cloning into {abspath}", file=sys.stderr)
            os.makedirs(os.path.dirname(abspath), exist_ok=True)
            # gh repo clone works in gh-authenticated sandboxes where plain
            # git clone is restricted (matches clone-lex-* convention).
            if gh.repo_clone(repo, abspath).returncode != 0:
                print(f"→ {repo}: clone FAILED", file=sys.stderr)
                rc = 1
    return rc


def _refresh_one(repo: str, abspath: str) -> bool:
    """fetch+reset to origin's default branch. Returns False on any failure."""
    head = proc.run(
        ["git", "-C", abspath, "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        check=False,
    )
    default = head.stdout.strip() if head.returncode == 0 and head.stdout.strip() else "main"
    default = default.rsplit("/", 1)[-1]  # strip refs/remotes/origin/ → branch name
    fetch = proc.run(["git", "-C", abspath, "fetch", "--quiet", "origin", default], check=False)
    if fetch.returncode != 0:
        return False
    return (
        proc.run(
            ["git", "-C", abspath, "reset", "--quiet", "--hard", f"origin/{default}"],
            check=False,
        ).returncode
        == 0
    )
