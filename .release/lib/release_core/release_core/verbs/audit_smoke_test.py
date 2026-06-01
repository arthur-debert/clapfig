"""audit-smoke-test — smoke-test the canonical PR loop end-to-end on a real repo.

Opens a no-op PR, verifies that the loop *starts*:
  - Copilot Review workflow run is created within 90s
  - The 'request' job adds Copilot as a reviewer (timeline event)
  - Required CI workflows are triggered on the PR
Then closes the PR (or keeps it with --keep).

This is "really works", not "should work" — verifies that the
config, secrets, and workflow files actually produce the expected
behavior in real GitHub Actions.

Usage:
  audit-smoke-test owner/repo
  audit-smoke-test owner/repo --keep        # don't close PR / delete branch
  audit-smoke-test owner/repo --base custom # base branch (default: main)

Exit codes:
  0  — smoke test passed (PR opened, Copilot fired, CI triggered)
  1  — smoke test failed (one or more checks below)
  64 — bad usage

Shell→Python migration (docs/proposals/shell-to-python.md): the gh-api/jq polling
of workflow runs + timeline moved into Python (gh.rest → parsed dicts, no jq).
The orchestration (clone/commit/push/PR open/close) is genuine side-effecting
glue and stays a sequence of proc/gh calls; the testable decisions (target-file
selection, run/timeline parsing, report verdict) are pure and fixture-tested.
"""

from __future__ import annotations

import datetime
import os
import shutil
import subprocess
import sys
import tempfile
import time

from .. import gh, proc

USAGE = __doc__ or ""

_SMOKE_CANDIDATES = ("CHANGELOG_UNRELEASED.md", "CHANGELOG.md", "README.md")


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


# --------------------------------------------------------------------------
# Pure helpers — fixture-tested, no network/side effects.
# --------------------------------------------------------------------------


def pick_target(clone_dir: str) -> str | None:
    """Choose a markdown file safe to no-op-touch. Prefer the changelog files,
    else the first non-node_modules *.md within depth 2. None if nothing found."""
    for candidate in _SMOKE_CANDIDATES:
        if os.path.isfile(os.path.join(clone_dir, candidate)):
            return candidate
    root_depth = clone_dir.rstrip("/").count(os.sep)
    for dirpath, _dirs, files in os.walk(clone_dir):
        if "node_modules" in dirpath.split(os.sep):
            continue
        if dirpath.rstrip("/").count(os.sep) - root_depth > 2:
            continue
        for name in sorted(files):
            if name.endswith(".md"):
                return os.path.relpath(os.path.join(dirpath, name), clone_dir)
    return None


def copilot_run_id(runs_payload: object) -> str:
    """First Copilot Review workflow-run id from an actions/runs payload, or ''."""
    runs = runs_payload.get("workflow_runs") if isinstance(runs_payload, dict) else None
    for run in runs or []:
        if isinstance(run, dict) and run.get("name") == "Copilot Review":
            rid = run.get("id")
            return str(rid) if rid is not None else ""
    return ""


def ci_run_names(runs_payload: object) -> str:
    """Sorted-unique non-Copilot workflow names from an actions/runs payload, CSV."""
    runs = runs_payload.get("workflow_runs") if isinstance(runs_payload, dict) else None
    names = {
        run["name"]
        for run in (runs or [])
        if isinstance(run, dict) and run.get("name") and run["name"] != "Copilot Review"
    }
    return ",".join(sorted(names))


def copilot_requested(timeline_payload: object) -> int:
    """Count of review_requested events naming Copilot in a PR timeline payload."""
    if not isinstance(timeline_payload, list):
        return 0
    return sum(
        1
        for ev in timeline_payload
        if isinstance(ev, dict)
        and ev.get("event") == "review_requested"
        and isinstance(ev.get("requested_reviewer"), dict)
        and ev["requested_reviewer"].get("login") == "Copilot"
    )


def render_report(
    repo: str, pr_num: str, run_id: str, requested: int, ci_runs: str
) -> tuple[str, int]:
    """Build the report block + fail count. Mirrors the bash report exactly."""
    lines = ["", f"=== smoke test results: {repo} PR #{pr_num} ==="]
    fails = 0

    if run_id:
        lines.append(f"  [PASS] {'copilot_review_fired':<30} run={run_id}")
    else:
        lines.append(f"  [FAIL] {'copilot_review_fired':<30} no run within 90s")
        fails += 1

    if requested > 0:
        lines.append(f"  [PASS] {'copilot_review_requested':<30} Copilot added as reviewer")
    else:
        lines.append(
            f"  [FAIL] {'copilot_review_requested':<30} no review_requested event for Copilot"
        )
        fails += 1

    if ci_runs:
        lines.append(f"  [PASS] {'ci_workflows_triggered':<30} {ci_runs}")
    else:
        lines.append(
            f"  [WARN] {'ci_workflows_triggered':<30} no non-copilot workflows on PR branch"
        )

    lines.append("")
    if fails > 0:
        lines.append(f"smoke test FAILED ({fails} check(s))")
    else:
        lines.append("smoke test PASSED")
    return "\n".join(lines), fails


# --------------------------------------------------------------------------
# Orchestration — genuine side-effecting glue (clone/commit/push/PR).
# --------------------------------------------------------------------------


def _run(cmd: list[str], *, cwd: str | None = None) -> subprocess.CompletedProcess[str]:
    return proc.run(cmd, cwd=cwd, check=False)


def main(argv: list[str]) -> int:  # noqa: C901 — linear orchestration mirrors the bash
    repo = ""
    keep = False
    base = ""

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--keep":
            keep = True
        elif arg == "--base":
            if i + 1 >= len(argv):
                return _usage_error("--base needs a value")
            i += 1
            base = argv[i]
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        elif arg.startswith("-"):
            return _usage_error(f"unknown arg: {arg}")
        elif not repo:
            repo = arg
        else:
            return _usage_error(f"extra arg: {arg}")
        i += 1

    if not repo:
        print("usage: audit-smoke-test owner/repo", file=sys.stderr)
        return 64

    # Validate repo access.
    if gh.repo_view(repo=repo, check=False).returncode != 0:
        print(f"error: cannot access {repo} via gh", file=sys.stderr)
        return 1

    # Default branch.
    if not base:
        try:
            repo_obj = gh.rest(f"repos/{repo}")
            base = repo_obj.get("default_branch", "") if isinstance(repo_obj, dict) else ""
        except gh.GhError:
            base = ""
    if not base:
        print("error: could not determine default branch", file=sys.stderr)
        return 1

    workdir = tempfile.mkdtemp(prefix="audit-smoke-")
    pr_num = ""
    try:
        clone = os.path.join(workdir, "clone")
        print(f"[smoke] cloning {repo} into {workdir}")
        clone_res = _run(
            ["git", "clone", "--depth=1", f"--branch={base}", f"https://github.com/{repo}", clone]
        )
        if clone_res.returncode != 0:
            print("[smoke] FAIL clone", file=sys.stderr)
            return 1

        target = pick_target(clone)
        if not target:
            print("error: no markdown file to touch", file=sys.stderr)
            return 1
        print(f"[smoke] targeting {target} for no-op edit")

        branch = f"audit-smoke-{int(time.time())}"
        if _run(["git", "checkout", "-b", branch], cwd=clone).returncode != 0:
            print("[smoke] FAIL checkout", file=sys.stderr)
            return 1

        stamp = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
        with open(os.path.join(clone, target), "a", encoding="utf-8") as fh:
            fh.write(f"\n<!-- audit-smoke-test {stamp} -->\n")

        _run(["git", "add", target], cwd=clone)
        _run(
            [
                "git",
                "-c",
                "user.email=audit-smoke@local",
                "-c",
                "user.name=audit-smoke-test",
                "commit",
                "-m",
                "chore: audit-smoke-test (no-op, will be closed)",
                "--no-verify",
            ],
            cwd=clone,
        )
        if _run(["git", "push", "-u", "origin", branch], cwd=clone).returncode != 0:
            print("[smoke] FAIL push", file=sys.stderr)
            return 1

        body = (
            "Automated smoke test from `bin/audit-smoke-test`. This PR will be "
            "closed automatically.\n\nVerifies the canonical PR loop starts: "
            "Copilot Review workflow fires + required checks trigger."
        )
        create = gh.pr_create(
            repo=repo,
            base=base,
            head=branch,
            title="audit-smoke-test (auto-generated, will be closed)",
            body=body,
        )
        pr_url = (create.stdout.strip().splitlines() or [""])[-1]
        pr_num = _last_int(pr_url)
        if not pr_num:
            # Surface BOTH streams: gh writes progress/URLs to stdout and the
            # actual error to stderr, so either may carry the diagnostic.
            print(
                f"[smoke] FAIL pr create:\n"
                f"STDOUT: {create.stdout.strip()}\n"
                f"STDERR: {create.stderr.strip()}",
                file=sys.stderr,
            )
            return 1
        print(f"[smoke] opened PR #{pr_num}: {pr_url}")

        return _verify(repo, branch, pr_num)
    finally:
        if not keep and pr_num:
            print(f"[smoke] closing PR #{pr_num}")
            gh.pr_close(
                pr_num,
                repo=repo,
                delete_branch=True,
                comment="audit-smoke-test complete",
            )
        if not keep:
            shutil.rmtree(workdir, ignore_errors=True)


def _verify(repo: str, branch: str, pr_num: str) -> int:
    # Check 1: Copilot Review workflow run created within 90s.
    print("[smoke] waiting for Copilot Review workflow run (up to 90s)...")
    run_id = ""
    for _ in range(18):
        try:
            payload = gh.rest(f"repos/{repo}/actions/runs?branch={branch}&per_page=20")
        except gh.GhError:
            payload = None
        run_id = copilot_run_id(payload)
        if run_id:
            break
        time.sleep(5)

    # Check 2: required CI workflows triggered.
    print("[smoke] checking other CI workflows on PR branch...")
    try:
        ci_payload = gh.rest(f"repos/{repo}/actions/runs?branch={branch}&per_page=20")
    except gh.GhError:
        ci_payload = None
    ci_runs = ci_run_names(ci_payload)

    # Check 3: Copilot review-request timeline event.
    print("[smoke] checking PR timeline for review-request event...")
    time.sleep(5)
    try:
        timeline = gh.rest(f"repos/{repo}/issues/{pr_num}/timeline?per_page=30")
    except gh.GhError:
        timeline = None
    requested = copilot_requested(timeline)

    report, fails = render_report(repo, pr_num, run_id, requested, ci_runs)
    print(report)
    return 1 if fails > 0 else 0


def _last_int(text: str) -> str:
    """Trailing run of digits in `text` (the PR number from a PR URL), or ''."""
    import re

    m = re.search(r"(\d+)\s*$", text)
    return m.group(1) if m else ""
