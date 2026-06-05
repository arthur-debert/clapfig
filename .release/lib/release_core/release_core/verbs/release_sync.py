"""Sync managed files from the release repo into the current consumer repo
using the build-directory + symlinks model (ADR-0001).

Usage:
  release-sync            # rebuild .release/ and symlinks
  release-sync --check    # exit non-zero if anything would change
  release-sync --dry-run  # show what would happen, write nothing
  release-sync --migrate  # remove real files at managed locations
                               # before creating symlinks (for adopting
                               # the new sync model on a repo that has
                               # files from the old sync as real files)

How it works:
  1. Detect Kind via detect-kind
  2. Resolve Capabilities (Kind manifest + consumer .release-sync.yaml)
  3. Materialize ALL managed files into .release/ from scratch
  4. For each file in .release/, ensure a relative symlink exists at
     the expected consumer location (path-mirror: strip template prefix)
  5. Walk the repo for broken symlinks pointing into .release/ and
     delete them

No state file. The filesystem is the state. Removals propagate
automatically because .release/ is rebuilt from scratch.

Source ref selection (first match wins):
  $RELEASE_REF if set
  origin/release/beta/<consumer-repo-name>
  origin/release/beta/<kind>
  origin/main

Exit codes:
  0  — clean (sync mode: rebuilt; check mode: no changes needed)
  1  — changes detected (--check only) or fatal error
  64 — bad usage
"""

from __future__ import annotations

import contextlib
import os
import shutil
import sys
import tempfile

from .. import gh, manifest, sync

# The --help body is the module docstring's header block, matching the bash
# `show_help() { sed -n '2,/^$/p' "$0" | sed -E 's/^# ?//'; }` byte-for-byte —
# the bash sed range runs from line 2 through the first blank line INCLUSIVE
# (the blank line before `set -euo pipefail`), so the output carries a trailing
# blank line. We reproduce that: docstring body + one trailing blank line.
USAGE = (__doc__ or "").strip("\n") + "\n\n"


def _err(msg: str) -> None:
    print(msg, file=sys.stderr)


def main(argv: list[str]) -> int:  # noqa: C901, PLR0911, PLR0912, PLR0915 — faithful port of a long linear bash
    # --- Arg parse (mirror the bash while/case; bad usage → 64) ----------
    mode = "sync"
    migrate = False
    for arg in argv:
        if arg == "--check":
            mode = "check"
        elif arg == "--dry-run":
            mode = "dryrun"
        elif arg == "--migrate":
            migrate = True
        elif arg in ("-h", "--help"):
            print(USAGE, end="")
            return 0
        else:
            _err(f"unknown arg: {arg}")
            print(USAGE, end="", file=sys.stderr)
            return 64

    # --- Guards ----------------------------------------------------------
    release_home = os.environ.get("RELEASE_HOME") or os.path.join(
        os.path.expanduser("~"), "release"
    )
    if not gh.is_git_worktree(release_home):
        _err(f"release-sync: $RELEASE_HOME='{release_home}' is not a git clone")
        return 1
    if shutil.which("yq") is None:
        _err("release-sync: yq is required (mikefarah/yq v4).")
        return 1

    try:
        repo_root = gh.git(["rev-parse", "--show-toplevel"])
    except Exception:
        _err("release-sync: not inside a git repo")
        return 1
    os.chdir(repo_root)

    # `cd "$RELEASE_HOME" && pwd -P` vs `pwd -P` — refuse to run inside HOME.
    release_real = os.path.realpath(release_home)
    repo_real = os.path.realpath(os.getcwd())
    if release_real == repo_real:
        _err("release-sync: refusing to run inside $RELEASE_HOME")
        return 1

    repo_name = os.path.basename(repo_root)
    try:
        kind = manifest.detect_kind(repo_root)
    except manifest.KindError:
        # detect-kind (the shim) prints "could not detect kind of <pwd>" + exit 1.
        # The bash `kind=$(detect-kind)` under `set -e` aborts release-sync with
        # detect-kind's own stderr already emitted. Reproduce that surface.
        print(f"could not detect kind of {repo_real}", file=sys.stderr)
        return 1

    # --- Ref selection ---------------------------------------------------
    release_ref = os.environ.get("RELEASE_REF") or None
    try:
        ref = sync.select_ref(release_home, repo_name, kind, release_ref)
    except sync.SyncError as exc:
        _err(str(exc))
        return 1

    ref_sha = gh.git_rev_parse(ref, cwd=release_home)

    if not sync._has_nonempty_line(
        gh.git_ls_tree(ref, f"templates/{kind}", cwd=release_home, dirs_only=True, name_only=True)
    ):
        _err(f"release-sync: ref '{ref}' has no templates/{kind}/ tree")
        return 1

    # --- Capability resolution -------------------------------------------
    # --migrate: rewrite a legacy `components:` field to `capabilities:`.
    if migrate and mode == "sync" and os.path.isfile(".release-sync.yaml"):
        _maybe_migrate_components_field()

    sync_yaml_text = None
    if os.path.isfile(".release-sync.yaml"):
        with open(".release-sync.yaml", encoding="utf-8", errors="replace") as fh:
            sync_yaml_text = fh.read()
    # A malformed .release-sync.yaml / Kind manifest.yaml drives yq to a parse
    # error, which yamlio surfaces as YamlError. Catch it at the CLI boundary and
    # exit non-zero with yq's message rather than letting a traceback escape — the
    # same hard-fail-with-clean-message contract release-drift-check enforces.
    from .. import yamlio

    try:
        caps = sync.resolve_capabilities(release_home, ref, kind, sync_yaml_text=sync_yaml_text)
    except yamlio.YamlError as exc:
        _err(f"release-sync: {exc}")
        return 1
    try:
        sync.validate_capabilities(release_home, ref, caps.names)
    except sync.SyncError as exc:
        _err(str(exc))
        return 1

    caps_display = " ".join(caps.names) if caps.names else "(none)"
    print(f"repo:         {repo_name}")
    print(f"kind:         {kind}")
    print(f"ref:          {ref} ({ref_sha})")
    print(f"manifest:     {caps.manifest_source}")
    print(f"capabilities: {caps_display}")
    print()

    # --- Build the plan + materialize into a sibling tempdir -------------
    plan = sync.build_plan(release_home, ref, kind, caps.names, repo_root=repo_real)

    # Sibling tempdir so the final mv is a same-filesystem rename (atomic).
    tmp_release = tempfile.mkdtemp(prefix=".release-build.", dir=".")
    swapped = False  # set once tmp_release is rename()d into place (consumed)
    try:
        sync.materialize(release_home, ref, ref_sha, plan, tmp_release)

        # --- Compute changes ---------------------------------------------
        file_diff, new_files = sync.diff_release(tmp_release, ".release")
        mirror = sync.compute_mirror(new_files, repo_real, tmp_release, migrate=migrate)
        claude = sync.decide_claude(repo_real, tmp_release)

        # --- Report ------------------------------------------------------
        _report(file_diff, mirror, claude)

        claude_change = 1 if claude.action in ("create", "inject", "refresh") else 0
        total_changes = (
            len(file_diff.added)
            + len(file_diff.modified)
            + len(file_diff.removed)
            + len(mirror.symlinks_to_create)
            + len(mirror.symlinks_to_remove)
            + len(mirror.migrated)
            + len(mirror.copies_to_write)
            + len(mirror.copies_to_remove)
            + claude_change
        )
        print()
        print(f"summary: {total_changes} changes, {len(mirror.conflicts)} conflicts")

        if mode == "check":
            # A conflict counts as divergence even with zero planned changes.
            return 0 if (total_changes == 0 and not mirror.conflicts) else 1
        if mode == "dryrun":
            return 0

        # --- Apply -------------------------------------------------------
        # Replace .release/ atomically: build new tree alongside, swap.
        if os.path.isdir(".release"):
            shutil.rmtree(".release")
        os.rename(tmp_release, ".release")
        swapped = True  # tmp_release consumed; the finally must not delete it

        _apply(mirror, claude)
    finally:
        # Mirror the bash EXIT trap: the build tree is cleaned on every path
        # that returns/raises BEFORE the swap (check, dry-run, error). After the
        # swap it IS .release/ and must survive. (_apply writes CLAUDE.md via its
        # own temp file that it cleans itself.)
        if not swapped:
            shutil.rmtree(tmp_release, ignore_errors=True)

    print()
    print("done.")
    return 0


def _maybe_migrate_components_field() -> None:
    """Mirror the --migrate `components:` → `capabilities:` rewrite. Only when
    the consumer's .release-sync.yaml has `components` but not `capabilities`."""
    from .. import yamlio

    try:
        data = yamlio.load(".release-sync.yaml")
    except yamlio.YamlError:
        return
    if not isinstance(data, dict):
        return
    if "components" in data and "capabilities" not in data:
        _err("migrating .release-sync.yaml: components: -> capabilities:")
        # yq -i '.capabilities = .components | del(.components)'
        from .. import proc

        proc.run(
            [
                "yq",
                "-i",
                ".capabilities = .components | del(.components)",
                ".release-sync.yaml",
            ]
        )


def _report(file_diff: sync.FileDiff, mirror: sync.MirrorPlan, claude: sync.ClaudeDecision) -> None:
    """Emit the report block byte-for-byte (the +file/~file/+link/=copy lines)."""
    print("=== .release/ changes ===")
    for f in file_diff.added:
        print(f"  +file     {f}")
    for f in file_diff.modified:
        print(f"  ~file     {f}")
    for f in file_diff.removed:
        print(f"  -file     {f}")

    print()
    print("=== symlink changes ===")
    for s in mirror.symlinks_to_create:
        print(f"  +link     {s}")
    for s in mirror.symlinks_to_remove:
        print(f"  -link     {s} (broken)")

    if mirror.copies_to_write or mirror.copies_to_remove:
        print()
        print("=== copy changes (real-file managed paths) ===")
        for f in mirror.copies_to_write:
            print(f"  =copy     {f}")
        for f in mirror.copies_to_remove:
            print(f"  -copy     {f} (stale managed copy)")

    if mirror.conflicts:
        print()
        print("=== conflicts (real file at managed location — symlink NOT created) ===")
        for f in mirror.conflicts:
            print(f"  !file     {f}")
        print()
        print("Resolve by removing the real file, then re-running sync (or re-run with --migrate).")

    if mirror.migrated:
        print()
        print("=== migrating (real file → symlink) ===")
        for f in mirror.migrated:
            print(f"  >file     {f} (will be deleted, then symlinked)")

    if claude.action != "none":
        print()
        print("=== CLAUDE.md orientation (#348) ===")
        if claude.action == "create":
            print(f"  +claude   {sync.CLAUDE_FILE} (create with managed orientation block)")
        elif claude.action == "inject":
            print(f"  ~claude   {sync.CLAUDE_FILE} (inject managed orientation block at top)")
        elif claude.action == "refresh":
            print(f"  ~claude   {sync.CLAUDE_FILE} (refresh managed orientation block)")
        elif claude.action == "skip-symlink":
            print(f"  !claude   {sync.CLAUDE_FILE} is a symlink — left untouched")


def _apply(mirror: sync.MirrorPlan, claude: sync.ClaudeDecision) -> None:
    """The apply phase: --migrate removals, symlink create/remove, managed-copy
    write/remove, and the CLAUDE.md write. Mirrors the bash `--- Apply ---`."""
    # If --migrate, delete real files at managed locations first.
    for f in mirror.migrated:
        _rm_f(f)

    # Create / update symlinks.
    for s in mirror.symlinks_to_create:
        link, _, target = s.partition(" -> ")
        d = os.path.dirname(link)
        if d:
            os.makedirs(d, exist_ok=True)
        if os.path.islink(link):
            os.remove(link)
        os.symlink(target, link)

    # Remove broken symlinks (paths are './…' relative to repo root).
    for link in mirror.symlinks_to_remove:
        os.remove(link)

    # Write managed copies (real files for paths GH can't dereference).
    for f in mirror.copies_to_write:
        d = os.path.dirname(f)
        if d:
            os.makedirs(d, exist_ok=True)
        if os.path.islink(f):
            os.remove(f)
        src = os.path.join(".release", f)
        if f.endswith((".yml", ".yaml")):
            with open(src, "rb") as sfh:
                body = sfh.read()
            with open(f, "wb") as dfh:
                dfh.write((sync.MANAGED_MARKER + "\n").encode("utf-8"))
                dfh.write(body)
        else:
            shutil.copyfile(src, f)
        # Preserve the executable bit from the source.
        if os.access(src, os.X_OK):
            st = os.stat(f)
            os.chmod(f, st.st_mode | 0o111)

    # Remove stale managed copies.
    for f in mirror.copies_to_remove:
        os.remove(f)

    # Write the consumer CLAUDE.md orientation block.
    if claude.action in ("create", "inject", "refresh"):
        # Atomic same-filesystem replace via a sibling temp file.
        assert claude.desired is not None
        fd, tmp = tempfile.mkstemp(prefix=sync.CLAUDE_FILE + ".tmp.", dir=".")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                fh.write(claude.desired)
            os.chmod(tmp, 0o644)
            os.replace(tmp, sync.CLAUDE_FILE)
        except BaseException:
            if os.path.exists(tmp):
                os.remove(tmp)
            raise


def _rm_f(path: str) -> None:
    """`rm -rf` — remove if present (file, symlink, or directory), ignore absence
    but surface real errors (permission/IO), like `rm -f` does for a file.

    A pre-existing managed dest is usually a real file (e.g. a stale hand-copied
    .claude/skills/<name>/SKILL.md). It can also be a real directory; remove that
    too so the managed symlink can take its place.

    Absence (FileNotFoundError) is ignored — matching `rm -f` — including the
    TOCTOU window where the dir vanishes between the isdir() check and the
    rmtree (a concurrent/CI race). But a real failure (permission/IO) must
    propagate rather than be silently swallowed (which would leave the path in
    place and make the later os.symlink fail with a confusing FileExistsError),
    so we do NOT pass ignore_errors=True; instead we suppress ONLY
    FileNotFoundError."""
    with contextlib.suppress(FileNotFoundError):
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path)
        else:
            os.remove(path)
