"""Detect consumer-side drift from canonical release-sync state.

"Drift" = a managed file was edited in the consumer instead of upstream
in release/ (a symlink replaced by a real file, a `.release/` file
hand-edited, a `# shellcheck disable` slipped into a managed script, or
extra keys added to `.release-sync.yaml`). All of these defeat the
"fix once, propagate via @v1" contract and get silently clobbered on
the next sync. This is the programmatic gate for the rules the
migrate-consumer-to-build-dir skill only documents in prose.

Crucially, drift is NOT the same as staleness. A consumer that is simply
behind canonical (release/main moved ahead of what it last synced) has
NOT drifted. We tell them apart by rebuilding against the exact revision
recorded in `.release/.release-sync-source` (the provenance marker, ADR-0002)
— not against a moving ref like v1/main. If rebuilding from the recorded
revision reproduces the committed tree, there is zero drift no matter how
far behind canonical the consumer is.

Usage:
  release-drift-check            # exit 1 if drift, 0 if clean
  release-drift-check --quiet    # only print on drift

Exit codes:
  0  — no drift (clean, or no marker yet → nothing to compare against)
  1  — drift detected (managed file diverged, or .release-sync.yaml over-override)
  2  — usage / internal error
  64 — bad usage

Requires release-sync + detect-kind on PATH and a $RELEASE_HOME clone
containing the recorded source revision's objects (in CI, check out
arthur-debert/release with fetch-depth: 0).
"""

from __future__ import annotations

import contextlib
import io
import os
import re
import shutil
import sys

from .. import gh, yamlio
from . import release_sync

# The --help body mirrors the bash `show_help() { sed -n '2,/^$/p' "$0" | sed -E
# 's/^# ?//'; }` byte-for-byte: lines 2 through the first blank line INCLUSIVE
# (the blank before `set -euo pipefail`), comment markers stripped. That range
# is the docstring header above through (and including) the blank line that
# follows the "Requires …" paragraph — i.e. the whole docstring plus one
# trailing blank line.
USAGE = (__doc__ or "").strip("\n") + "\n\n"

SOURCE_MARKER = ".release/.release-sync-source"
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def _err(msg: str) -> None:
    print(msg, file=sys.stderr)


def main(argv: list[str]) -> int:  # noqa: C901, PLR0911, PLR0912 — faithful port of a linear bash gate
    # --- Arg parse (mirror the bash while/case; bad usage → 64) ----------
    quiet = False
    for arg in argv:
        if arg == "--quiet":
            quiet = True
        elif arg in ("-h", "--help"):
            print(USAGE, end="")
            return 0
        else:
            _err(f"unknown arg: {arg}")
            print(USAGE, end="", file=sys.stderr)
            return 64

    # --- Guards (release-sync + yq on PATH) ------------------------------
    # `command -v release-sync` / `command -v yq` → exit 2 when missing.
    if shutil.which("release-sync") is None:
        _err("release-drift-check: release-sync not on PATH")
        return 2
    if shutil.which("yq") is None:
        _err("release-drift-check: yq is required (mikefarah/yq v4)")
        return 2

    try:
        repo_root = gh.git(["rev-parse", "--show-toplevel"])
    except Exception:
        _err("release-drift-check: not inside a git repo")
        return 2
    os.chdir(repo_root)

    # --- No marker → nothing to compare against; skip, don't fail --------
    if not os.path.isfile(SOURCE_MARKER):
        if not quiet:
            print(
                f"release-drift-check: no {SOURCE_MARKER} — repo not on a "
                "marker-aware sync; skipping drift gate (re-sync to backfill)."
            )
        return 0

    # sha = first `^[0-9a-f]{40}$` line, CR-stripped.
    sha = _read_marker_sha(SOURCE_MARKER)
    if not sha:
        _err(f"release-drift-check: {SOURCE_MARKER} has no 40-char SHA line")
        return 2

    drift = False
    report = ""

    # --- 1. Managed-file drift -------------------------------------------
    # Rebuild against the recorded revision. release-sync --check is read-only
    # (exits before applying) and reports any add/modify/remove/conflict in the
    # managed surface. Against the marker SHA, a clean consumer produces zero
    # changes; any change is drift, not staleness.
    check_out, managed_clean = _run_sync_check(sha)
    if not managed_clean:
        # release-sync --check exits 1 both for "changes detected" and for fatal
        # errors (e.g. the recorded SHA's templates are unreachable). Surface the
        # raw output either way so a human/agent can tell which it was.
        drift = True
        report += f"Managed-file drift (rebuilt against release@{sha[:12]}):\n{check_out}\n"

    # --- 2. .release-sync.yaml over-override -----------------------------
    # Only `capabilities` is an honored consumer knob. Any other top-level key
    # is per-repo rule tuning that release-sync silently ignores — so it never
    # shows up as managed-file drift, but it IS forbidden divergence.
    if os.path.isfile(".release-sync.yaml"):
        try:
            extra = _extra_sync_keys(".release-sync.yaml")
        except yamlio.YamlError as exc:
            # Malformed YAML is a developer bug, not a transient failure. The bash
            # let yq's nonzero exit trip `set -e` and abort hard; we surface yq's
            # message and exit nonzero (2 = internal error) rather than silently
            # reporting "no extra keys".
            _err(f"release-drift-check: {exc}")
            return 2
        if extra:
            drift = True
            bullets = "".join(f"  - {k}\n" for k in extra)
            report += (
                ".release-sync.yaml has keys beyond `capabilities` "
                f"(per-repo rule tuning is not allowed):\n{bullets}"
            )

    # --- Verdict ---------------------------------------------------------
    if not drift:
        if not quiet:
            print(f"release-drift-check: clean — managed surface matches release@{sha[:12]}.")
        return 0

    _err(
        "release-drift-check: DRIFT DETECTED.\n"
        "\n"
        f"{report}"
        "\n"
        "This belongs UPSTREAM in arthur-debert/release, not in this consumer.\n"
        "release-sync rebuilds .release/ from scratch, so a local edit to a managed\n"
        "file is both wrong (it won't propagate to other consumers) and futile (the\n"
        "next sync clobbers it). Fix the template in release/, cut a PATCH, advance\n"
        "@v1, and re-sync — don't patch around it here. See the migrate-consumer\n"
        'skill\'s "Bucket A" triage.'
    )
    return 1


def _read_marker_sha(path: str) -> str | None:
    """Mirror `tr -d '\\r' < marker | grep -E '^[0-9a-f]{40}$' | head -n 1`:
    the first line that is exactly a 40-char lowercase hex SHA (CRs stripped)."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            line = raw.replace("\r", "").rstrip("\n")
            if _SHA_RE.match(line):
                return line
    return None


def _run_sync_check(sha: str) -> tuple[str, bool]:
    """Rebuild against the recorded SHA via release-sync's --check, in-process.

    Mirrors the bash `check_out=$(RELEASE_REF="$sha" release-sync --check 2>&1)
    && managed_clean=1 || managed_clean=0`: combined stdout+stderr is captured
    and the exit code decides cleanliness. We dispatch release_sync.main rather
    than re-shell the shim — the logic lives in release_core — with RELEASE_REF
    pinned to the marker SHA for the call only. release_sync.main re-resolves and
    chdir()s to the same repo root we are already in (a no-op), so cwd is stable.
    """
    buf = io.StringIO()
    prev_ref = os.environ.get("RELEASE_REF")
    os.environ["RELEASE_REF"] = sha
    try:
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            try:
                rc = release_sync.main(["--check"])
            except SystemExit as exc:  # defensive: a guard that exits rather than returns
                rc = exc.code if isinstance(exc.code, int) else 1
            except Exception as exc:  # a fatal rebuild error mirrors `set -e` aborting the subshell
                print(str(exc), file=sys.stderr)
                rc = 1
    finally:
        if prev_ref is None:
            os.environ.pop("RELEASE_REF", None)
        else:
            os.environ["RELEASE_REF"] = prev_ref
    # `command_substitution=$(...)` strips trailing newlines.
    return buf.getvalue().rstrip("\n"), rc == 0


def _extra_sync_keys(path: str) -> list[str]:
    """Mirror `yq '(. // {}) | keys | .[] | select(. != "capabilities")'`:
    top-level keys other than `capabilities`. An empty/null document yields no
    keys; a genuine YAML *parse* error propagates (yamlio raises) so malformed
    config hard-fails exactly like the bash `set -e` on yq's nonzero exit."""
    data = yamlio.load(path)  # YamlError on a parse failure → propagates (hard fail)
    if not isinstance(data, dict):
        # `(. // {})` coerces empty/null/scalar to a map → no keys.
        return []
    return [str(k) for k in data if str(k) != "capabilities"]
