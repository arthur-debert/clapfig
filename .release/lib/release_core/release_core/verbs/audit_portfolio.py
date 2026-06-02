"""audit-portfolio — audit every managed repo against the "good state" baseline.

Source of truth: managed-repos.yaml at the release/ repo root — the ONLY
fleet list. There is no auto-discovery: ruleset/gh-api discovery caused
recurring scope bugs and was removed. To audit a one-off set, pass
--repos.

Usage:
  audit-portfolio                              # read managed-repos.yaml
  audit-portfolio --repos arthur-debert/dodot,lex-fmt/lex   # explicit set
  audit-portfolio --json                       # one JSON object per line
  audit-portfolio --only-failing               # hide all-green repos in table

Exit codes:
  0  — every audited repo all-green
  1  — at least one repo has FAIL
  2  — at least one repo has WARN (no FAILs)
  64 — bad usage

Shell→Python migration (docs/proposals/shell-to-python.md): drops the
fork-per-repo `audit-repo` subprocess + grep-counting of its rows; it imports
the audit_repo verb and aggregates its in-memory results directly. The --json
stream (one audit-repo object per line), the summary table, the per-repo
conformance %, and the exit codes are preserved.
"""

from __future__ import annotations

import os
import shutil
import sys

from .. import yamlio
from . import audit_repo

USAGE = __doc__ or ""


def _usage_block() -> str:
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def _usage_error(msg: str) -> int:
    print(msg, file=sys.stderr)
    return 64


def _manifest_path() -> str:
    """bin/../managed-repos.yaml, resolved via the shim's exported script dir.

    Mirrors the bash `script_dir/../managed-repos.yaml`. Falls back to the cwd
    (the repo root in practice, since the verb is release-only)."""
    script_dir = os.environ.get("AUDIT_PORTFOLIO_SCRIPT_DIR")
    if script_dir:
        return os.path.normpath(os.path.join(script_dir, "..", "managed-repos.yaml"))
    return "managed-repos.yaml"


def _manifest_repos(manifest: str) -> list[str]:
    """Every repo from `.projects[][].repo`, in manifest declaration order."""
    data = yamlio.load(manifest) or {}
    projects = data.get("projects") or {}
    repos: list[str] = []
    for entries in projects.values():
        for entry in entries or []:
            if entry.get("repo"):
                repos.append(entry["repo"])
    return repos


def conformance_pct(passes: int, fails: int, warns: int) -> int:
    """passes / (passes+fails+warns) as an int %, excluding SKIPs. 0 if none apply."""
    applicable = passes + fails + warns
    return (100 * passes // applicable) if applicable > 0 else 0


def _counts(results: list[tuple[str, str, str]]) -> tuple[int, int, int, int]:
    """(fails, warns, passes, skips) for a repo's audit rows."""
    fails = sum(1 for s, _, _ in results if s == "FAIL")
    warns = sum(1 for s, _, _ in results if s == "WARN")
    passes = sum(1 for s, _, _ in results if s == "PASS")
    skips = sum(1 for s, _, _ in results if s == "SKIP")
    return fails, warns, passes, skips


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash modes
    explicit_repos = ""
    json_mode = False
    only_failing = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--repos":
            if i + 1 >= len(argv):
                return _usage_error("--repos needs a value")
            i += 1
            explicit_repos = argv[i]
        elif arg == "--json":
            json_mode = True
        elif arg == "--only-failing":
            only_failing = True
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        else:
            return _usage_error(f"unknown arg: {arg}")
        i += 1

    # Repo set: explicit --repos, else the managed-repos.yaml fleet. No discovery.
    if explicit_repos:
        repos = [r for r in explicit_repos.split(",") if r]
    else:
        manifest = _manifest_path()
        if not os.path.isfile(manifest):
            print(f"error: {manifest} not found; pass --repos", file=sys.stderr)
            return 1
        if shutil.which("yq") is None:
            print(
                f"error: yq required to read {manifest} (mikefarah/yq v4)",
                file=sys.stderr,
            )
            return 1
        repos = _manifest_repos(manifest)
        if not json_mode:
            print(
                f"auditing managed repos from {manifest} ({len(repos)} repos)",
                file=sys.stderr,
            )

    if not repos:
        print("no onboarded repos found", file=sys.stderr)
        return 1

    if not json_mode:
        print(f"auditing {len(repos)} repo(s)...", file=sys.stderr)

    # Run audit per repo. Each row: (ec, fails, warns, passes, skips, conf, repo, results)
    summary_rows: list[tuple] = []
    detail_repos: list[str] = []
    total_fails = 0
    total_warns = 0

    for r in repos:
        results = audit_repo.audit(r)
        if json_mode:
            print(audit_repo.render_json(r, results))
            continue
        ec = audit_repo.exit_code(results)
        fails, warns, passes, skips = _counts(results)
        conf = conformance_pct(passes, fails, warns)
        summary_rows.append((ec, fails, warns, passes, skips, conf, r, results))
        if fails > 0 or warns > 0:
            detail_repos.append(r)
        total_fails += fails
        total_warns += warns

    if json_mode:
        return 0

    return _render_table(summary_rows, detail_repos, repos, total_fails, total_warns, only_failing)


_STATUS_BY_EC = {0: "green", 1: "FAIL", 2: "warn"}


def _render_table(
    summary_rows: list[tuple],
    detail_repos: list[str],
    repos: list[str],
    total_fails: int,
    total_warns: int,
    only_failing: bool,
) -> int:
    print()
    print(f"{'REPO':<40} {'FAIL':>5} {'WARN':>5} {'CONF':>5} STATUS")
    print(f"{'----':<40} {'----':>5} {'----':>5} {'----':>5} ------")

    total_passes = 0
    total_applicable = 0
    results_by_repo: dict[str, list] = {}
    for ec, fails, warns, passes, _skips, conf, r, results in summary_rows:
        results_by_repo[r] = results
        total_passes += passes
        total_applicable += passes + fails + warns
        if only_failing and fails == 0 and warns == 0:
            continue
        status = _STATUS_BY_EC.get(ec, f"?({ec})")
        # Mirror the bash printf '%-40s %5s %5s %4s%% %s\n' — CONF is 4-wide + '%'.
        print(f"{r:<40} {fails:>5} {warns:>5} {conf:>4}% {status}")

    # Detail view: re-render audit-repo --quiet for problem repos.
    if detail_repos:
        print()
        print("=== details for repos with failures or warnings ===")
        for r in detail_repos:
            print()
            print(audit_repo.render_human(r, results_by_repo[r], quiet=True))

    portfolio_conf = (100 * total_passes // total_applicable) if total_applicable > 0 else 0
    print(
        f"summary: {len(repos)} repo(s), {total_fails} failure(s), "
        f"{total_warns} warning(s), portfolio conformance: {portfolio_conf}% "
        f"({total_passes}/{total_applicable} applicable rows pass)"
    )

    if total_fails > 0:
        return 1
    if total_warns > 0:
        return 2
    return 0
