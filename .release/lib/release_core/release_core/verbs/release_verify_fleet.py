"""release-verify-fleet — pre-flight lint sweep across the whole portfolio.

Verifies that a candidate release revision, once synced into every
managed consumer, still passes the canonical lint gate. Run this BEFORE
`release-advance-v1` to catch a commons/lint regression in release's own
tree instead of discovering it one consumer at a time after @v1 moves.

It is HERMETIC: it clones the fleet into a throwaway root (NOT your ~/h
checkouts), syncs each from the candidate ref, and runs
`lefthook run pre-commit --all-files`. It never mutates your working
repos. Re-runs reuse the clones (pass --refresh to update them).

This is the "checkout all repos, release-sync them, try to commit" idea:
real consumer files (the genuine edge cases), zero synthetic fixtures.

Usage:
  release-verify-fleet [--ref <ref>] [--root <dir>] [--refresh] [--only <owner/name,...>]

  --ref <ref>     release revision to sync FROM (default: HEAD of this
                  release checkout). Tests the code you have right here.
  --root <dir>    where to clone the fleet (default:
                  /tmp/release-fleet-verify-$USER, per-user to avoid
                  permission clashes on shared machines).
  --refresh       fetch+reset existing fleet clones before syncing.
  --only <list>   restrict to a comma-separated subset of owner/name.

Caveat: the gate only catches what the local lint tools can see. The
canonical commons gate SKIPS a missing tool (exits 0), so run this on a
box with shellcheck/yamllint/prettier/markdownlint/yq installed (or read
it as a complement to CI, not a replacement).

Exit codes:
  0  — every consumer's gate passed (or skipped tools cleanly)
  1  — at least one consumer's gate FAILED
  2  — setup/dependency error
  64 — bad usage
"""

from __future__ import annotations

import os
import shutil
import sys

from .. import gh, proc

# The --help body mirrors the bash `show_help() { sed -n '2,/^$/p' "$0" | sed -E
# 's/^# ?//'; }`: lines 2..first-blank of the header comment. That range is the
# docstring's first paragraph (down to the blank line before "Usage:"); since
# this docstring carries no migration note, the help text is the full docstring
# — the same content the bash printed from its header comment.
USAGE = __doc__ or ""


def _usage_block() -> str:
    """The help body, byte-for-byte the bash `show_help` output.

    The bash `show_help() { sed -n '2,/^$/p' "$0" | sed -E 's/^# ?//'; }` prints
    the header comment from line 2 through the first truly-blank line INCLUSIVE,
    so its output carries ONE trailing blank line. We reproduce that: the
    docstring body + a single trailing newline (the empty terminating line)."""
    return USAGE.strip("\n") + "\n"


def _usage_error(msg: str) -> int:
    print(msg, file=sys.stderr)
    print(_usage_block(), file=sys.stderr)
    return 64


def _release_home() -> str:
    """RELEASE_HOME = <shim bin/>/.. — the script_dir/.. the bash computed.

    The shim exports VERIFY_FLEET_SCRIPT_DIR (its own realpath'd bin/ dir) so
    this resolves identically regardless of cwd. Falls back to ~/release."""
    script_dir = os.environ.get("VERIFY_FLEET_SCRIPT_DIR")
    if script_dir:
        return os.path.realpath(os.path.join(script_dir, ".."))
    return os.path.join(os.path.expanduser("~"), "release")


def main(argv: list[str]) -> int:  # noqa: C901, PLR0911, PLR0912, PLR0915 — faithful port of a long linear bash
    # --- Arg parse (mirror the bash while/case; bad usage → 64) ----------
    ref = "HEAD"
    user = os.environ.get("USER") or "shared"
    root = f"/tmp/release-fleet-verify-{user}"  # noqa: S108 — matches the bash default path
    refresh = False
    only = ""

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--ref":
            if i + 1 >= len(argv):
                return _usage_error("release-verify-fleet: --ref needs a value")
            i += 1
            ref = argv[i]
        elif arg == "--root":
            if i + 1 >= len(argv):
                return _usage_error("release-verify-fleet: --root needs a value")
            i += 1
            root = argv[i]
        elif arg == "--refresh":
            refresh = True
        elif arg == "--only":
            if i + 1 >= len(argv):
                return _usage_error("release-verify-fleet: --only needs a value")
            i += 1
            only = argv[i]
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        else:
            return _usage_error(f"release-verify-fleet: unknown arg: {arg}")
        i += 1

    release_home = _release_home()
    os.environ["RELEASE_HOME"] = release_home

    # --- Dependency guard ------------------------------------------------
    for tool in ("managed-repos", "release-sync", "detect-kind", "lefthook", "yq", "gh", "git"):
        if shutil.which(tool) is None:
            print(f"release-verify-fleet: {tool} not on PATH", file=sys.stderr)
            return 2

    # Resolve the candidate ref to a concrete SHA up front so the report is
    # unambiguous and every consumer syncs from the identical revision.
    if not gh.git_rev_parse_verify(ref, cwd=release_home):
        print(
            f"release-verify-fleet: bad --ref '{ref}' in {release_home}",
            file=sys.stderr,
        )
        return 2
    ref_sha = gh.git_rev_parse(ref, cwd=release_home)
    print(
        f"release-verify-fleet: syncing fleet from release@{ref_sha[:12]} ({ref}) into {root}",
        file=sys.stderr,
    )

    # An --only subset (comma-separated) becomes positional filter args shared
    # by both managed-repos calls, so the clone is scoped too.
    subset = [s for s in only.split(",") if s] if only else []

    # --- Phase 1: materialize the fleet (hermetic — into $root, never ~/h) ---
    print("==> cloning/refreshing fleet", file=sys.stderr)
    clone_args = ["--clone"]
    if refresh:
        clone_args.append("--refresh")
    clone = proc.run(
        ["managed-repos", *clone_args, *subset],
        env={"REPOS_ROOT": root},
        check=False,
        capture_output=False,
    )
    if clone.returncode != 0:
        print(
            "release-verify-fleet: fleet clone reported failures (continuing with what cloned)",
            file=sys.stderr,
        )

    # --- Phase 2: per-consumer sync + gate -------------------------------
    print("repo\tkind\tsync\tgate")
    overall = 0
    seen = 0

    paths = proc.run(
        ["managed-repos", "--paths", *subset],
        env={"REPOS_ROOT": root},
    )
    for line in paths.stdout.splitlines():
        if not line:
            continue
        # Mirror `IFS=$'\t' read -r repo abspath found`: split into exactly three
        # fields, the last absorbing any further tabs (a tab in abspath is absurd
        # but read tolerates it, so the faithful port must too — and maxsplit
        # avoids a ValueError unpack on a >3-field line).
        repo, abspath, found = line.split("\t", 2)
        seen += 1

        if found != "found":
            print(f"{repo}\t-\tmissing\tskipped")
            overall = 1
            continue

        kind = _detect_kind(abspath)

        if _run_sync(abspath, ref_sha, release_home):
            sync = "ok"
        else:
            print(f"{repo}\t{kind}\tFAILED\tskipped")
            overall = 1
            continue

        if _run_gate(abspath):
            gate = "pass"
        else:
            gate = "FAIL"
            overall = 1
        print(f"{repo}\t{kind}\t{sync}\t{gate}")

    print(file=sys.stderr)
    print(
        f"release-verify-fleet: {seen} consumer(s) swept from release@{ref_sha[:12]}.",
        file=sys.stderr,
    )
    if overall != 0:
        print(
            "release-verify-fleet: FAILURES above. Logs under "
            f"{root}: <repo>/.verify-sync.log (sync) and "
            "<repo>/.verify-gate.log (gate).",
            file=sys.stderr,
        )
    else:
        print(
            "release-verify-fleet: all consumers pass the gate against this revision.",
            file=sys.stderr,
        )
    return overall


def _detect_kind(abspath: str) -> str:
    """`(cd "$abspath" && detect-kind 2>/dev/null || echo "?")` — the kind, or '?'."""
    result = proc.run(["detect-kind"], cwd=abspath, check=False)
    out = result.stdout.strip()
    if result.returncode != 0 or not out:
        return "?"
    return out


def _run_sync(abspath: str, ref_sha: str, release_home: str) -> bool:
    """Run release-sync in the consumer clone, env-pinned to the candidate SHA.

    Combined stdout+stderr is written to <abspath>/.verify-sync.log (the bash
    `>"$abspath/.verify-sync.log" 2>&1`). Returns True on a zero exit."""
    result = proc.run(
        ["release-sync"],
        cwd=abspath,
        env={"RELEASE_REF": ref_sha, "RELEASE_HOME": release_home},
        check=False,
    )
    _write_log(os.path.join(abspath, ".verify-sync.log"), result)
    return result.returncode == 0


def _run_gate(abspath: str) -> bool:
    """Run the canonical lint gate in the consumer clone.

    Combined stdout+stderr is written to <abspath>/.verify-gate.log. Returns
    True on a zero exit (the gate passed / skipped missing tools cleanly)."""
    result = proc.run(
        ["lefthook", "run", "pre-commit", "--all-files"],
        cwd=abspath,
        check=False,
    )
    _write_log(os.path.join(abspath, ".verify-gate.log"), result)
    return result.returncode == 0


def _write_log(path: str, result) -> None:
    """Mirror the bash `>LOG 2>&1`: stdout then stderr, combined, into LOG."""
    with open(path, "w", encoding="utf-8", errors="replace") as fh:
        fh.write(result.stdout or "")
        fh.write(result.stderr or "")
