"""release-lex — orchestrate a coordinated release across the lex-fmt
repo chain (comms -> lex/tree-sitter-lex -> vscode/nvim/lexed).

Layer 1 of the cross-repo release automation (lex-fmt/lex#640). Walks the
dependency chain, calls each repo's `scripts/release/*` primitives (Layer 0),
and drives the actual git + GitHub state transitions: branch, version bump,
commit, PR, admin-merge, tag, wait for release CI to finish before moving to
dependents.

Each repo provides these primitives under `scripts/release/`:
  get-current-version          — bare semver from this repo's manifest
  get-commits-since-release    — `<sha> <subject>` lines since latest tag
  update-release <new-version> — bumps manifest, deps, CHANGELOG; git-adds
  trigger-release <new-version>— fires the repo's release CI
  should-release               — (status mode only) cascade decision

Shell->Python migration (docs/proposals/shell-to-python.md): release-lex is a
release-only tool (a real file in bin/, NOT synced to consumers), so its shim
is the contract's variant (b). The orchestration sequence, stdout, exit codes,
and the dry-run / --only / --status gates are preserved byte-for-byte — pinned
by tests/release-lex/release-lex.bats and the pure-decision pytest unit tests.

The live multi-repo orchestration (fetch/checkout/pull/commit/push/reset/
submodule, gh pr create/merge, gh run list/watch) is genuine side-effecting
glue and is NOT unit-tested (it requires live repos + GitHub — that is the
script's whole point). What IS pure and tested: the github-slug map,
compute_new_version, arg parsing + validation exit codes, the --only filter,
the PR-number / run-id extractors, and the status-line rendering.

Usage:
  release-lex <bump-kind> \\
    --comms <path> --lex <path> --tree-sitter <path> \\
    --vscode <path> --nvim <path> --lexed <path> \\
    [--dry-run] [--only <name>[,<name>...]]

<bump-kind> is one of: patch | minor | major | <X.Y.Z>

Exit codes:
  0  — all attempted releases cut successfully
  1  — at least one repo failed mid-flight (orchestrator stops there)
  64 — bad usage
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time

from .. import gh, proc

# Walk order (dependency-respecting). Each repo is included only if its path
# was supplied via flag.
ORDER = ("comms", "lex", "tree-sitter", "vscode", "nvim", "lexed")

_GITHUB_SLUGS = {
    "comms": "lex-fmt/comms",
    "lex": "lex-fmt/lex",
    "tree-sitter": "lex-fmt/tree-sitter-lex",
    "vscode": "lex-fmt/vscode",
    "nvim": "lex-fmt/nvim",
    "lexed": "lex-fmt/lexed",
}

USAGE = """\
Usage:
  release-lex <bump-kind> \\
    --comms <path> --lex <path> --tree-sitter <path> \\
    --vscode <path> --nvim <path> --lexed <path> \\
    [--dry-run] [--only <name>[,<name>...]]

  release-lex --status \\
    --comms <path> --lex <path> --tree-sitter <path> \\
    --vscode <path> --nvim <path> --lexed <path>

<bump-kind>:  patch | minor | major | <X.Y.Z>
--dry-run:    print everything but make no real state changes
--only:       restrict to a subset of repos (still dependency-ordered)
--status:     read-only — run should-release in each repo and print
              a one-line answer per repo. Useful answer to "what
              would cascade if I cut comms now?"
"""


# --------------------------------------------------------------------------
# Pure helpers (unit-tested).
# --------------------------------------------------------------------------


def github_slug_for(name: str) -> str:
    """The lex-fmt GitHub slug for a repo key (empty string if unknown —
    matches the bash `case` with no default arm)."""
    return _GITHUB_SLUGS.get(name, "")


def compute_new_version(current: str, bump: str) -> str:
    """Return the new semver string. ``bump`` is patch/minor/major or an
    explicit X.Y.Z (returned as-is, minus any leading 'v').

    Mirrors the bash: an explicit version matches `^[0-9]+\\.[0-9]+\\.[0-9]+`
    (a *prefix* match — any trailing pre-release/build is kept verbatim) and is
    returned with a leading 'v' stripped; otherwise the part is bumped against
    ``current`` (also 'v'-stripped). An unrecognized bump yields '' (the bash
    `case` had no default arm)."""
    current = current[1:] if current.startswith("v") else current
    if re.match(r"^[0-9]+\.[0-9]+\.[0-9]+", bump):
        return bump[1:] if bump.startswith("v") else bump
    parts = current.split(".")
    # Mirror bash `IFS='.' read -r major minor patch`: only the first three
    # fields are used; missing ones default to empty (bash leaves them unset).
    major = parts[0] if len(parts) > 0 else ""
    minor = parts[1] if len(parts) > 1 else ""
    patch = parts[2] if len(parts) > 2 else ""
    if bump == "patch":
        return f"{major}.{minor}.{int(patch) + 1}"
    if bump == "minor":
        return f"{major}.{int(minor) + 1}.0"
    if bump == "major":
        return f"{int(major) + 1}.0.0"
    return ""


def render_status_line(key: str, current: str, rc: int, decision: str) -> str:
    """Render one status-mode line. Mirrors the bash printf formats exactly:
      rc 0 -> '⚠ ...'  (release would happen)
      rc 1 -> '✓ ...'  (no release needed)
      other -> '✗ should-release exited <rc>: ...'
    The label is left-padded to 18 cols (printf '%-18s'); the version slot is
    'v' + the version left-padded to 8 (printf 'v%-8s')."""
    label = f"{key:<18}"
    ver = f"v{current:<8}"
    if rc == 0:
        return f"{label} {ver} ⚠ {decision}"
    if rc == 1:
        return f"{label} {ver} ✓ {decision}"
    return f"{label} {ver} ✗ should-release exited {rc}: {decision}"


def parse_only(only: str) -> list[str]:
    """Split a comma-separated --only value the way bash `IFS=',' read -ra` did."""
    if not only:
        return []
    return only.split(",")


def _looks_like_version(bump: str) -> bool:
    """Mirror the bash `*.*.*` glob arm in the bump-kind validation: at least
    two literal dots (so the string has >=3 dot-separated pieces, each possibly
    empty). Note this is LOOSER than compute_new_version's `^[0-9]+\\.…` regex —
    the validation accepted anything dot-dotted; faithfully preserved."""
    return bump.count(".") >= 2


def _extract_pr_number(gh_output: str) -> str:
    """Pull the PR number out of `gh pr create` output. Mirrors the bash
    `grep -oE 'pull/[0-9]+' | grep -oE '[0-9]+$'`: find a `pull/<n>` token and
    return its trailing number. Empty string if none (the `|| true` path)."""
    m = re.search(r"pull/([0-9]+)", gh_output)
    return m.group(1) if m else ""


def _first_database_id(runs_json: str) -> str:
    """`.[0].databaseId // empty` over `gh run list --json databaseId` output:
    the first run's databaseId as a string, or '' if absent/empty/unparseable."""
    import json

    text = runs_json.strip()
    if not text:
        return ""
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return ""
    if not isinstance(data, list) or not data:
        return ""
    val = data[0].get("databaseId")
    if val is None:
        return ""
    return str(val)


# --------------------------------------------------------------------------
# Argument parsing + validation.
# --------------------------------------------------------------------------


class _Usage(Exception):
    """Signal a usage error (exit 64) with a message for stderr."""

    def __init__(self, message: str = "") -> None:
        self.message = message
        super().__init__(message)


def _parse_args(argv: list[str]) -> dict:
    """Parse argv into a config dict, or raise _Usage / SystemExit.

    Mirrors the bash control flow precisely:
      - no args -> usage() to stdout, exit 64
      - first arg '--status' -> status mode, BUMP_KIND='status'; else the first
        positional is BUMP_KIND
      - remaining: --<repo> <path>, --dry-run, --only <val>, --status
      - unknown arg -> 'release-lex: unknown arg: <arg>' to stderr, exit 64
    """
    if len(argv) < 1:
        print(USAGE, end="")
        raise SystemExit(64)

    rest = list(argv)
    status_mode = False
    if rest[0] == "--status":
        status_mode = True
        bump_kind = "status"
        rest = rest[1:]
    else:
        bump_kind = rest[0]
        rest = rest[1:]

    dry_run = False
    only = ""
    repos: dict[str, str] = {}

    i = 0
    while i < len(rest):
        arg = rest[i]
        if arg in ("--comms", "--lex", "--tree-sitter", "--vscode", "--nvim", "--lexed"):
            key = arg[2:]
            # Bash `shift 2` past the end leaves an empty value, not an error.
            repos[key] = rest[i + 1] if i + 1 < len(rest) else ""
            i += 2
        elif arg == "--dry-run":
            dry_run = True
            i += 1
        elif arg == "--only":
            only = rest[i + 1] if i + 1 < len(rest) else ""
            i += 2
        elif arg == "--status":
            status_mode = True
            i += 1
        else:
            raise _Usage(f"release-lex: unknown arg: {arg}")

    return {
        "status_mode": status_mode,
        "bump_kind": bump_kind,
        "dry_run": dry_run,
        "only": only,
        "repos": repos,
    }


def _validate(cfg: dict) -> int | None:
    """Post-parse validation; returns an exit code to abort on, or None to
    proceed. Mirrors the bash validation order:
      1. bad bump-kind (non-status mode) -> 64
      2. no repos -> 64
      3. each repo: dir exists + 4 primitives executable -> 1
    """
    if not cfg["status_mode"]:
        bump = cfg["bump_kind"]
        if bump in ("patch", "minor", "major") or _looks_like_version(bump):
            pass
        else:
            print(
                f"release-lex: bad bump-kind: {bump} (want patch|minor|major|X.Y.Z)",
                file=sys.stderr,
            )
            return 64

    if not cfg["repos"]:
        print("release-lex: no repo paths supplied", file=sys.stderr)
        return 64

    # Validate paths + primitives. Bash iterated `"${!REPOS[@]}"` (an unordered
    # associative-array key set); we iterate in the stable ORDER so the first
    # failure reported is deterministic. Either way the first failure aborts.
    keys = [k for k in ORDER if k in cfg["repos"]]
    keys += [k for k in cfg["repos"] if k not in ORDER]
    for key in keys:
        path = cfg["repos"][key]
        if not os.path.isdir(path):
            print(f"release-lex: not a directory: {path} (for --{key})", file=sys.stderr)
            return 1
        for prim in (
            "get-current-version",
            "get-commits-since-release",
            "update-release",
            "trigger-release",
        ):
            prim_path = os.path.join(path, "scripts", "release", prim)
            if not (os.path.isfile(prim_path) and os.access(prim_path, os.X_OK)):
                print(
                    f"release-lex: {key} at {path} is missing scripts/release/{prim}",
                    file=sys.stderr,
                )
                print(
                    f"  (Layer 0 must be merged in lex-fmt/{github_slug_for(key)} first)",
                    file=sys.stderr,
                )
                return 1
    return None


def _is_allowed(name: str, allowed: list[str], only_raw: str) -> bool:
    """Mirror the bash `is_allowed`: with no --only, everything is allowed;
    otherwise only names present in the comma-split --only list."""
    if not only_raw:
        return True
    return name in allowed


# --------------------------------------------------------------------------
# Side-effecting orchestration (faithful glue — NOT unit-tested).
# --------------------------------------------------------------------------


def _run(cmd: list[str], dry_run: bool, *, cwd: str | None = None) -> None:
    """echo + execute, OR echo only if --dry-run. Mirrors the bash `run()`:
    prints `  $ <space-joined cmd>` then runs it (inheriting the parent's
    stdout/stderr) when not in dry-run. A nonzero exit raises (bash `set -e`)."""
    print("  $ " + " ".join(cmd))
    if not dry_run:
        subprocess.run(cmd, cwd=cwd, check=True)  # noqa: S603 — cmd is a constructed list


def _release_one(key: str, cfg: dict) -> int:
    """Cut one repo's release. Returns 0 on success/skip, 1 on failure.
    Faithful port of the bash `release_one` — same sequence, same stdout."""
    path = cfg["repos"][key]
    dry_run = cfg["dry_run"]
    bump_kind = cfg["bump_kind"]
    gh_repo = github_slug_for(key)

    print()
    print(f"═══ {key} ({gh_repo}) at {path} ═══")

    os.chdir(path)
    _run(["git", "fetch", "origin"], dry_run)
    _run(["git", "checkout", "main"], dry_run)
    _run(["git", "pull", "--ff-only"], dry_run)
    # Submodule init for consumers of comms (no-op if absent).
    if os.path.isfile(".gitmodules"):
        _run(["git", "submodule", "update", "--init", "--recursive"], dry_run)

    # `get-commits-since-release || true`: tolerate a nonzero exit, keep stdout.
    res = proc.run(["./scripts/release/get-commits-since-release"], check=False)
    commits = res.stdout
    if not commits.strip():
        print(f"  ↳ no new commits since latest release tag; skipping {key}")
        return 0
    # `echo "$commits" | grep -c .` — count non-empty lines.
    count = sum(1 for line in commits.split("\n") if line)
    print(f"  ↳ {count} commit(s) since latest release")

    current = proc.out(["./scripts/release/get-current-version"])
    new = compute_new_version(current, bump_kind)
    print(f"  ↳ version: {current} → {new}")

    branch = f"release/v{new}"
    _run(["git", "checkout", "-b", branch], dry_run)
    _run(["./scripts/release/update-release", new], dry_run)
    _run(["git", "commit", "-m", f"chore: release v{new}"], dry_run)
    _run(["git", "push", "-u", "origin", branch], dry_run)
    print(f"  ↳ branch pushed: https://github.com/{gh_repo}/tree/{branch}")

    if dry_run:
        print("  ↳ dry-run: skipping PR creation, admin-merge, tag, CI wait")
        # Roll back the local branch so we don't accumulate dry-run state.
        _run(["git", "checkout", "main"], dry_run)
        _run(["git", "branch", "-D", branch], dry_run)
        return 0

    # Open PR + admin-merge. The ruleset on main blocks direct push, so PR is
    # the only mechanism — `--admin` bypasses the review requirement for this
    # batch of automated releases.
    pr_out = gh.pr_create(
        repo=gh_repo,
        title=f"chore: release v{new}",
        body="Cut by `release-lex` (Layer 1). Part of lex-fmt/lex#640.",
    ).stdout
    pr = _extract_pr_number(pr_out)
    if not pr:
        print("  ✗ gh pr create did not return a PR number", file=sys.stderr)
        return 1
    print(f"  ↳ PR #{pr} opened")
    gh.pr_merge(pr, repo=gh_repo, squash=True, delete_branch=True, admin=True)
    print(f"  ↳ PR #{pr} admin-merged")

    # Fast-forward to the merge commit on main, then delegate to the repo's own
    # `trigger-release` primitive (Layer 0). `git reset --hard origin/main`
    # (not `pull --ff-only`) is deliberate: some repos' pre-commit hooks leave
    # the tree dirty (regenerated parser.c / electron build artifacts), which
    # would fail `pull --ff-only`. Reset is safe — the bump is already merged to
    # origin/main and any leftover tree state is regeneratable artefact.
    subprocess.run(["git", "fetch", "origin"], check=True)  # noqa: S603
    subprocess.run(["git", "checkout", "main"], check=True)  # noqa: S603
    subprocess.run(["git", "reset", "--hard", "origin/main"], check=True)  # noqa: S603
    if os.path.isfile(".gitmodules"):
        subprocess.run(  # noqa: S603
            ["git", "submodule", "update", "--init", "--recursive"], check=True
        )

    commit_sha = proc.out(["git", "rev-parse", "HEAD"])
    _run(["./scripts/release/trigger-release", new], dry_run)
    print(f"  ↳ release triggered for v{new}")

    # Find the release-CI run just triggered. Filter by commit SHA (consistent
    # for both tag-push and workflow_dispatch — no clock skew) and restrict to
    # release.yml to ignore unrelated CI on the same push.
    time.sleep(8)
    runs = gh.run_list(
        repo=gh_repo,
        workflow_eq="release.yml",
        commit=commit_sha,
        limit=1,
        json_fields=["databaseId"],
    )
    run_id = _first_database_id(runs.stdout)
    if not run_id:
        if runs.returncode != 0:
            # gh failed outright: surface BOTH streams (gh splits progress/JSON
            # across stdout and the error onto stderr) before the generic line.
            print(
                f"  ✗ gh run list failed:\n"
                f"STDOUT: {runs.stdout.strip()}\n"
                f"STDERR: {runs.stderr.strip()}",
                file=sys.stderr,
            )
        print(
            f"  ✗ could not find release CI run for v{new} (commit {commit_sha})",
            file=sys.stderr,
        )
        print(
            f"    inspect manually: gh run list --repo {gh_repo} --workflow=release.yml",
            file=sys.stderr,
        )
        return 1
    print(f"  ↳ watching release CI run {run_id}...")
    gh.run_watch(run_id, repo=gh_repo, exit_status=True)
    print(f"  ✓ release CI complete for {key} v{new}")
    return 0


def _status_one(key: str, cfg: dict) -> None:
    """Read-only: fetch remote state, run should-release, print one line.
    Faithful port of the bash `status_one` (a subshell there; here we
    save/restore cwd to preserve the same isolation)."""
    path = cfg["repos"][key]
    saved = os.getcwd()
    try:
        os.chdir(path)
        # `git fetch --quiet origin 2>/dev/null || true` — best-effort, ignored.
        proc.run(["git", "fetch", "--quiet", "origin"], check=False)
        # The bash `git diff --quiet HEAD` probe had only a `:` no-op branch —
        # purely informational, no output, no effect. Dropped.
        current_res = proc.run(["./scripts/release/get-current-version"], check=False)
        current = current_res.stdout.strip() if current_res.returncode == 0 else "?"
        decision_res = proc.run(["./scripts/release/should-release"], check=False)
        # Bash `2>&1`: should-release's stderr is folded into the captured value.
        decision = (decision_res.stdout + decision_res.stderr).strip()
        rc = decision_res.returncode
        print(render_status_line(key, current, rc, decision))
    finally:
        os.chdir(saved)


# --------------------------------------------------------------------------
# Entry point.
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    # The bash had no explicit -h/--help arm; with no args it printed usage and
    # exited 64. We preserve the no-arg path and additionally treat a leading
    # -h/--help as a help request (exit 0) — a benign superset for parity with
    # the other migrated verbs. Flagged in the PR body.
    if argv and argv[0] in ("-h", "--help"):
        print(USAGE, end="")
        return 0

    try:
        cfg = _parse_args(argv)
    except SystemExit as exc:
        return int(exc.code) if exc.code is not None else 0
    except _Usage as exc:
        if exc.message:
            print(exc.message, file=sys.stderr)
        return 64

    abort = _validate(cfg)
    if abort is not None:
        return abort

    if cfg["status_mode"]:
        print("Cascade status (read-only — runs should-release in each repo):")
        print()
        for key in ORDER:
            if cfg["repos"].get(key):
                _status_one(key, cfg)
        print()
        print("Legend: ✓ no release needed  ⚠ release would happen  ✗ error")
        return 0

    if cfg["dry_run"]:
        print("release-lex: dry-run mode — no commits, pushes, merges, or tags will be made")

    allowed = parse_only(cfg["only"])
    for key in ORDER:
        if cfg["repos"].get(key) and _is_allowed(key, allowed, cfg["only"]):
            try:
                rc = _release_one(key, cfg)
            except (proc.ProcError, subprocess.CalledProcessError, gh.GhError):
                # bash `set -e`: any failed command aborts the whole run with a
                # nonzero status. Mirror that — stop the walk, exit 1.
                return 1
            if rc != 0:
                return rc

    print()
    print("release-lex: all attempted releases complete.")
    return 0
