"""release-lex — orchestrate a coordinated release across the lex-fmt
repo chain (comms -> lex/tree-sitter-lex -> vscode/nvim/lexed).

Layer 1 of the cross-repo release automation (lex-fmt/lex#640). Walks the
dependency chain and drives each repo's release by invoking `release-cut`
(resolved from the MAINTAINER's PATH — the release repo's bin/) directly in each
repo's cwd. It no longer calls each repo's `bin/release` shim: that shim just
`exec`s the same `release-cut`, and it is missing on stale chain repos
(tree-sitter-lex, nvim — mains behind), which stalled the cascade. release-cut
reads cwd's manifest + dispatches cwd's release.yml, so a cwd-local call is
per-repo-correct AND self-contained (no dependency on each target repo's tooling
being current). There are no longer any per-repo `scripts/release/*` primitives
— those were retired (the feat/retire-scripts-dir line).

The should-release decision is computed GENERICALLY here via plain git (commits
since the last final release tag), NOT via a per-repo `bin/diff-since-release`.
That tool was absent for the manifest-less Kinds (docs-site / tree-sitter /
nvim-plugin — 3 of the 6 lex-chain repos) and diverged across the others, so the
cascade died at the first repo lacking it. The generic decision replicates the
old `diff-since-release` contract exactly (see `decide_release` below): commits
since the last NON-prerelease tag reachable from HEAD; no final tags ⇒ the
no-tags case (a first release is human-driven). Release is dispatched by the
maintainer's `release-cut`, run in each repo's cwd, with an EXPLICIT version
derived from that same latest final tag (TAG-AUTHORITATIVE — see below):

  release-cut <X.Y.Z>     — resolved from the maintainer's PATH (the release
                            repo's bin/), invoked with the repo as cwd. We
                            DERIVE the explicit X.Y.Z here by applying the run's
                            bump-kind to the latest final TAG decide_release
                            computed (e.g. v0.10.8 + patch → 0.10.9), and dispatch
                            THAT exact version — NOT the bump-kind. release-cut then
                            DISPATCHES cwd's `.github/workflows/release.yml` with
                            our explicit version. CI (the reusable per-Kind release
                            workflow) does the actual bump + CHANGELOG roll +
                            commit + tag + build + GitHub Release. We call
                            release-cut directly rather than the repo's
                            `bin/release` shim (which only execs release-cut and
                            is missing on stale chain repos) so the maintainer-run
                            cascade is self-contained.

TAG-AUTHORITATIVE version (why we dispatch an explicit X.Y.Z, not a bump-kind):
release-lex already DECIDES off the latest final git tag (generic git, no per-repo
tooling). If we instead dispatched `release-cut <bump-kind>`, release-cut would
recompute the new version from the repo's MANIFEST — which is wrong wherever the
manifest has drifted from the tag. The vscode case: package.json froze at
0.4.1-rc.1 ~25 releases ago while the real version is the tag v0.10.8, so a
manifest-driven `patch` bump yields 0.4.2 (wrong) instead of 0.10.9. So we apply
the bump-kind to the TAG via release_core.version and dispatch the explicit result,
making the cascade robust to manifest drift fleet-wide. Same self-contained pattern
as dropping per-repo diff-since-release / bin/release. An explicit-X.Y.Z bump-kind
passes through unchanged (no tag math).

The old primitive→responsibility mapping, for the record:
  get-current-version       -> release-cut reads it from the Kind manifest.
  get-commits-since-release -> the generic `git log <last-final-tag>..HEAD`.
  should-release            -> that log is non-empty (commits since last final).
  update-release            -+ both fold into `release-cut`: the bump + CHANGELOG
  trigger-release           -+ roll + commit + tag now happen IN CI, dispatched
                               by release-cut. There is no longer a local
                               "bump files + git add" step to commit/PR/merge,
                               so the old per-repo "branch -> update-release ->
                               commit -> PR -> admin-merge -> trigger-release"
                               tail collapses into a single `release-cut`
                               dispatch + a `gh run watch` on the resulting
                               release.yml run. See the PR body for the design
                               note on why the local PR/admin-merge mechanics
                               are gone (CI owns the mutation now).

release-lex is a release-only tool (a real file in bin/, NOT synced to
consumers). The orchestration sequence, stdout, exit codes, and the dry-run /
--only / --status gates are preserved where they still have meaning — pinned by
tests/release-lex/release-lex.bats and the pure-decision pytest unit tests.

The live multi-repo orchestration (fetch/checkout/pull/submodule, `release-cut`
dispatch, `gh run list/watch`) is genuine side-effecting glue and is NOT
unit-tested (it requires live repos + GitHub — that is the script's whole
point). What IS pure and tested: the github-slug map, arg parsing + validation
exit codes, the --only filter, the run-id extractor, the generic git
should-release decision (last-final-tag selection + log over a mocked proc
layer), and the status-line rendering.

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
import shutil
import subprocess
import sys
import time

from .. import gh, proc, version

# The maintainer-side dispatch tool. release-lex runs from the maintainer's
# release clone, so `release-cut` is on the maintainer's PATH (the release repo's
# bin/ dir). We invoke it DIRECTLY in each repo's cwd rather than the repo's
# `bin/release` shim — release-cut reads cwd's manifest + dispatches cwd's
# release.yml, so calling it per-repo-cwd is per-repo-correct, and it drops the
# dependency on each target repo carrying a current `bin/release` (which is
# missing on stale chain repos — tree-sitter-lex, nvim — whose mains lag). This
# is the same self-contained pattern already applied to the should-release
# decision (generic git, no per-repo bin/diff-since-release).
RELEASE_CUT = "release-cut"

# should-release decision states (the generic git decision, computed by
# `decide_release`). Replaces the old `diff-since-release` exit-code contract —
# the decision is now computed in-process from git, so there is no external
# tool's exit code to interpret. The four states preserve the EXACT semantics
# the old per-Kind `diff-since-release` drove:
#   RELEASE   — final tag found + commits since it (the old rc 0 + non-empty log)
#   UPTODATE  — final tag found + NO commits since it (the old rc 0 + empty log)
#   NOTAGS    — no final (non-prerelease) tag reachable from HEAD; a first
#               release is human-driven (the old rc 1 / NO_TAGS exit)
#   ERROR     — a genuine git failure (corrupt repo, unborn HEAD, bad ref); MUST
#               be surfaced loudly, never masked as "nothing to release" (the old
#               rc > 1, e.g. 128, under `set -e`)
RELEASE = "release"
UPTODATE = "uptodate"
NOTAGS = "notags"
ERROR = "error"

# git exits 128 on the failures we want to surface loudly (corrupt repo, unborn
# HEAD, bad ref). Used as the synthetic process exit code when a decision is
# ERROR but the underlying rc wasn't captured (it always is — kept for clarity).
GIT_ERROR_RC = 128

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
--status:     read-only — compute the should-release decision (git commits
              since the last final release tag) in each repo and print a
              one-line answer per repo. Useful answer to "what
              would cascade if I cut comms now?"
"""


# --------------------------------------------------------------------------
# Pure helpers (unit-tested).
# --------------------------------------------------------------------------


def github_slug_for(name: str) -> str:
    """The lex-fmt GitHub slug for a repo key (empty string if unknown)."""
    return _GITHUB_SLUGS.get(name, "")


def latest_final_tag(tag_lines: str) -> str:
    """The last FINAL (non-prerelease) release tag from `git tag --list 'v*'
    --sort=-version:refname --merged HEAD` output, or '' if there is none.

    Replicates the old `diff-since-release` selection EXACTLY: tags are already
    highest-version-first; we drop any prerelease tag (anything containing `-`,
    e.g. `v1.0.0-rc.1`) because pre-releases share the `## [Unreleased]`
    changelog scope with the final they lead up to — the relevant diff is against
    the last *final*. The first surviving line is the answer. A repo with only
    prerelease tags therefore yields '' (the no-final-tags case)."""
    for line in tag_lines.splitlines():
        tag = line.strip()
        if tag and "-" not in tag:
            return tag
    return ""


class Decision:
    """The generic should-release decision for one repo (computed by
    ``decide_release`` from plain git). ``state`` is one of RELEASE / UPTODATE /
    NOTAGS / ERROR; ``count`` is the commit count (only meaningful for RELEASE);
    ``tag`` is the last final tag ('' for NOTAGS/ERROR); ``rc`` is the git exit
    code that drove an ERROR (0 otherwise); ``stderr`` carries git's error text
    for an ERROR so the caller can surface it loudly."""

    def __init__(self, state: str, *, count: int = 0, tag: str = "", rc: int = 0, stderr: str = ""):
        self.state = state
        self.count = count
        self.tag = tag
        self.rc = rc
        self.stderr = stderr


def decide_release(path: str) -> Decision:
    """Compute the should-release decision for the repo at ``path`` GENERICALLY,
    via plain git — no per-repo `bin/diff-since-release` needed.

    Steps (replicating the old `diff-since-release` contract):
      1. `git tag --list 'v*' --sort=-version:refname --merged HEAD`. A nonzero
         exit is a genuine git failure (corrupt repo / unborn HEAD) → ERROR,
         surfaced loudly. NEVER read as "no tags".
      2. Pick the last FINAL (non-prerelease) tag. None → NOTAGS (a first release
         is human-driven; we never guess a first version).
      3. `git --no-pager log --oneline <tag>..HEAD`. A nonzero exit is a genuine
         git failure → ERROR. Empty stdout → UPTODATE. Non-empty → RELEASE with
         the commit count.
    """
    tags = gh.git_tag_list_merged("v*", cwd=path)
    if tags.returncode != 0:
        return Decision(ERROR, rc=tags.returncode or GIT_ERROR_RC, stderr=tags.stderr or "")
    tag = latest_final_tag(tags.stdout)
    if not tag:
        return Decision(NOTAGS)
    log = gh.git_log_oneline(f"{tag}..HEAD", cwd=path)
    if log.returncode != 0:
        return Decision(ERROR, tag=tag, rc=log.returncode or GIT_ERROR_RC, stderr=log.stderr or "")
    count = sum(1 for ln in log.stdout.splitlines() if ln.strip())
    if count > 0:
        return Decision(RELEASE, count=count, tag=tag)
    return Decision(UPTODATE, tag=tag)


def render_status_line(key: str, decision: Decision) -> str:
    """Render one status-mode line from a :class:`Decision`.
      RELEASE   -> '⚠ would release: N commit(s) since <tag>'
      UPTODATE  -> '✓ up to date (no commits since <tag>)'
      NOTAGS    -> '✗ no final release tags yet (first release is human-driven)'
      ERROR     -> '✗ should-release decision FAILED (git exited <rc>)'
    Only NOTAGS means "no tags yet"; an ERROR is a genuine git failure (corrupt
    repo, bad ref) and must not be reported as "no tags". The label is
    left-padded to 18 cols for column alignment."""
    label = f"{key:<18}"
    if decision.state == ERROR:
        return f"{label} ✗ should-release decision FAILED (git exited {decision.rc})"
    if decision.state == NOTAGS:
        return f"{label} ✗ no final release tags yet (first release is human-driven)"
    if decision.state == RELEASE:
        return f"{label} ⚠ would release: {decision.count} commit(s) since {decision.tag}"
    return f"{label} ✓ up to date (no commits since {decision.tag})"


def parse_only(only: str) -> list[str]:
    """Split a comma-separated --only value the way bash `IFS=',' read -ra` did."""
    if not only:
        return []
    return only.split(",")


def _looks_like_version(bump: str) -> bool:
    """The loose `*.*.*` validation arm: at least two literal dots (so the
    string has >=3 dot-separated pieces). Looser than a strict semver check —
    release-cut itself does the strict validation when it runs."""
    return bump.count(".") >= 2


def next_version(bump_kind: str, tag: str) -> str:
    """The explicit `X.Y.Z` version release-cut should dispatch, derived from the
    latest FINAL git ``tag`` (the one ``decide_release`` already computed), NOT
    from the repo's manifest.

    This is the tag-authoritative fix (release#... option A): release-lex DECIDES
    off the latest final tag, so it must also DERIVE the next version from that
    same tag and dispatch it explicitly. Letting `release-cut <bump-kind>` recompute
    from the manifest is wrong wherever the manifest has drifted from the tag — the
    vscode case, where `package.json` froze at 0.4.1-rc.1 ~25 releases ago while the
    real version is the tag v0.10.8, so a manifest-driven `patch` bump yields 0.4.2
    instead of 0.10.9.

      - ``bump_kind`` is one of patch|minor|major: apply it to ``tag`` via
        release_core.version (parse strips the leading 'v' + any prerelease, bump
        increments, fmt renders X.Y.Z). e.g. ('patch', 'v0.10.8') -> '0.10.9'.
      - ``bump_kind`` already an explicit X.Y.Z: pass it through unchanged (the
        operator named the exact version; no tag math).

    Only ever called for the RELEASE decision, where ``tag`` is a real final tag.
    The NOTAGS case never reaches here (a first release is human-driven — we never
    guess a version)."""
    if bump_kind not in ("patch", "minor", "major"):
        # Explicit X.Y.Z (already validated loose-semver by _validate) — dispatch
        # it verbatim; the tag is irrelevant when the operator named the version.
        return bump_kind
    return version.fmt(version.bump(version.parse(tag), bump_kind))


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
            raw = rest[i + 1] if i + 1 < len(rest) else ""
            # Resolve to an ABSOLUTE path ONCE, here at parse time, so every
            # later os.chdir() / git -C is absolute and order-independent. The
            # walk chdir's into each of the 6 repos in turn; a relative path
            # would otherwise resolve against the PREVIOUS repo's dir after the
            # first chdir (e.g. `comms` becomes `comms/comms`), breaking the
            # cascade for relative input. Empty stays empty so _validate still
            # reports a clean "not a directory" / missing-arg error.
            repos[key] = os.path.abspath(raw) if raw else ""
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
    proceed:
      1. bad bump-kind (non-status mode) -> 64
      2. no repos -> 64
      3. cut mode only: `release-cut` is on the maintainer's PATH -> 1 (checked
         ONCE, up front — it is the maintainer-side dispatch tool, not a per-repo
         dep). --status needs no repo tool at all.
      4. each repo: dir exists -> 1.
    The should-release decision is computed generically via git and the dispatch
    goes through the maintainer's `release-cut`, so NO per-repo bin/ tool
    (bin/release, bin/diff-since-release) is required in any repo / mode — they
    were absent / stale on chain repos whose mains lag.
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

    # Cut mode dispatches via the maintainer's `release-cut` (run in each repo's
    # cwd), so require it on PATH ONCE here rather than a bin/release per repo.
    # Resolve to an ABSOLUTE path now: dispatch runs after os.chdir into each
    # repo, so a relative PATH entry (`.`/`bin`) would otherwise re-resolve
    # against the changed cwd and break. Same spirit as the #404 abs-path fix.
    if not cfg["status_mode"]:
        resolved = shutil.which(RELEASE_CUT)
        if resolved is None:
            print(
                f"release-lex: {RELEASE_CUT} not on PATH — add the release repo's bin/ to PATH",
                file=sys.stderr,
            )
            return 1
        cfg["release_cut_path"] = resolved

    # Validate paths. We iterate in the stable ORDER so the first failure
    # reported is deterministic. No per-repo bin/ tool is required anymore.
    keys = [k for k in ORDER if k in cfg["repos"]]
    keys += [k for k in cfg["repos"] if k not in ORDER]
    for key in keys:
        path = cfg["repos"][key]
        if not os.path.isdir(path):
            print(f"release-lex: not a directory: {path} (for --{key})", file=sys.stderr)
            return 1
    return None


def _repo_name(key: str) -> str:
    """Bare repo name (slug minus the `lex-fmt/` owner) for error messages."""
    slug = github_slug_for(key)
    return slug.split("/", 1)[1] if "/" in slug else (slug or key)


def _is_allowed(name: str, allowed: list[str], only_raw: str) -> bool:
    """With no --only, everything is allowed; otherwise only names present in
    the comma-split --only list."""
    if not only_raw:
        return True
    return name in allowed


# --------------------------------------------------------------------------
# Side-effecting orchestration (faithful glue — NOT unit-tested).
# --------------------------------------------------------------------------


def _run(cmd: list[str], dry_run: bool, *, cwd: str | None = None) -> None:
    """echo + execute, OR echo only if --dry-run. Prints `  $ <cmd>` then runs
    it (inheriting the parent's stdout/stderr) when not in dry-run. A nonzero
    exit raises (CalledProcessError)."""
    print("  $ " + " ".join(cmd))
    if not dry_run:
        subprocess.run(cmd, cwd=cwd, check=True)  # noqa: S603 — cmd is a constructed list


def _release_one(key: str, cfg: dict) -> int:
    """Cut one repo's release. Returns 0 on success/skip, 1 on failure.

    New model (post scripts/release retirement): the bump + CHANGELOG roll +
    commit + tag all happen IN CI, dispatched by the maintainer's `release-cut`
    (run in the repo's cwd — NOT the repo's `bin/release` shim, which is missing
    on stale chain repos). There is no local file mutation to
    branch/commit/PR/admin-merge anymore, so this is now: refresh main -> decide
    generically via git (commits since the last final release tag) ->
    `release-cut <bump>` in the repo cwd (dispatch release.yml) -> watch the
    resulting CI run.
    """
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

    # should-release decision: commits since the last final release tag (generic
    # git — no per-repo bin/diff-since-release).
    decision = decide_release(path)
    # An ERROR is a genuine git failure (corrupt repo, bad ref — exits 128).
    # NEVER mask it as "nothing to release": a silent skip stalls the cascade.
    if decision.state == ERROR:
        print(
            f"  ✗ should-release decision FAILED for {key} (git exit {decision.rc}); aborting\n"
            f"    {decision.stderr.strip()}",
            file=sys.stderr,
        )
        return decision.rc
    if decision.state in (NOTAGS, UPTODATE):
        if decision.state == NOTAGS:
            print(f"  ↳ no final release tags yet; skipping {key}")
        else:
            print(f"  ↳ no new commits since {decision.tag}; skipping {key}")
        return 0
    print(f"  ↳ {decision.count} commit(s) since {decision.tag}")

    # TAG-AUTHORITATIVE version: derive the explicit X.Y.Z by applying the run's
    # bump-kind to the latest final TAG decide_release already computed — NOT from
    # the repo's manifest. We then dispatch `release-cut <X.Y.Z>` (the exact
    # version) rather than `release-cut <bump-kind>`, so the cascade is robust to
    # manifest drift fleet-wide (vscode's package.json froze at 0.4.1-rc.1 ~25
    # releases ago while its real version is the tag v0.10.8 → patch must be
    # 0.10.9, not the manifest-driven 0.4.2). An explicit-X.Y.Z bump-kind passes
    # through unchanged.
    try:
        cut_version = next_version(bump_kind, decision.tag)
    except ValueError as exc:
        print(
            f"  ✗ failed to parse tag {decision.tag!r} as semver: {exc}",
            file=sys.stderr,
        )
        return 1
    if bump_kind in ("patch", "minor", "major"):
        # Tag math actually happened — show the derivation.
        print(f"  ↳ next version {cut_version} (from {decision.tag} + {bump_kind})")
    else:
        # Explicit X.Y.Z passed straight through; no tag math, so don't claim any.
        print(f"  ↳ next version {cut_version} (explicit; tag {decision.tag} unused)")

    # `release-cut <X.Y.Z>` (maintainer's PATH tool, run in the repo cwd)
    # dispatches cwd's release.yml with the EXACT version we computed. CI does the
    # bump + CHANGELOG roll + commit + tag + build + GitHub Release. We call it
    # directly rather than the repo's `bin/release` shim so the cascade doesn't
    # depend on each target repo's tooling being current.
    if dry_run:
        print(f"  $ {RELEASE_CUT} {cut_version}")
        print("  ↳ dry-run: skipping release-cut dispatch + CI wait")
        return 0

    print(f"  $ {RELEASE_CUT} {cut_version}")
    # Use the absolute path resolved in _validate (before any os.chdir) and
    # route through the centralized subprocess chokepoint (proc.run).
    release_cut_cmd = cfg.get("release_cut_path", RELEASE_CUT)
    cut = proc.run([release_cut_cmd, cut_version], check=False, capture_output=False)
    if cut.returncode != 0:
        print(f"  ✗ {RELEASE_CUT} {cut_version} failed (exit {cut.returncode})", file=sys.stderr)
        return 1
    print(f"  ↳ release.yml dispatched for {key} ({cut_version})")

    # Find the release-CI run release-cut just dispatched and watch it. Filter
    # to release.yml and take the most recent run (dispatch is near-instant;
    # the brief sleep lets the run register before we query).
    time.sleep(8)
    runs = gh.run_list(
        repo=gh_repo,
        workflow_eq="release.yml",
        limit=1,
        json_fields=["databaseId"],
    )
    run_id = _first_database_id(runs.stdout)
    if not run_id:
        if runs.returncode != 0:
            print(
                f"  ✗ gh run list failed:\n"
                f"STDOUT: {runs.stdout.strip()}\n"
                f"STDERR: {runs.stderr.strip()}",
                file=sys.stderr,
            )
        print(
            f"  ✗ could not find dispatched release CI run for {key}",
            file=sys.stderr,
        )
        print(
            f"    inspect manually: gh run list --repo {gh_repo} --workflow=release.yml",
            file=sys.stderr,
        )
        return 1
    print(f"  ↳ watching release CI run {run_id}...")
    gh.run_watch(run_id, repo=gh_repo, exit_status=True)
    print(f"  ✓ release CI complete for {key}")
    return 0


def _status_one(key: str, cfg: dict) -> None:
    """Read-only: fetch remote state, compute the generic git should-release
    decision, print one line. (A subshell-equivalent: we save/restore cwd to
    preserve isolation.)"""
    path = cfg["repos"][key]
    saved = os.getcwd()
    try:
        os.chdir(path)
        # Best-effort fetch so the tag/log view is current; ignore failures.
        proc.run(["git", "fetch", "--quiet", "origin"], check=False)
        decision = decide_release(path)
        print(render_status_line(key, decision))
        # An ERROR is a genuine git failure — echo its stderr so an operator
        # scanning --status output doesn't miss the cause.
        if decision.state == ERROR and decision.stderr.strip():
            print(f"    {decision.stderr.strip()}", file=sys.stderr)
    finally:
        os.chdir(saved)


# --------------------------------------------------------------------------
# Entry point.
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
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
        print("Cascade status (read-only — commits since last final tag, per repo):")
        print()
        for key in ORDER:
            if cfg["repos"].get(key):
                _status_one(key, cfg)
        print()
        print("Legend: ✓ up to date  ⚠ release would happen  ✗ error / no tags")
        return 0

    if cfg["dry_run"]:
        print("release-lex: dry-run mode — no dispatches will be made")

    allowed = parse_only(cfg["only"])
    for key in ORDER:
        if cfg["repos"].get(key) and _is_allowed(key, allowed, cfg["only"]):
            try:
                rc = _release_one(key, cfg)
            except (proc.ProcError, subprocess.CalledProcessError, gh.GhError):
                # Any failed command aborts the whole run with a nonzero status.
                return 1
            if rc != 0:
                return rc

    print()
    print("release-lex: all attempted releases complete.")
    return 0
