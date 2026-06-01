"""sweep-github-policy — drop the canonical policy + setup files into a repo.

Drop the canonical policy + setup files into the current repo, based on
its detected stack.

Usage:
  sweep-github-policy [--stack <stack>] [--force]

- Auto-detects kind via detect-kind (override with --stack)
- For each template file: creates if missing; reports if already present
  with different content (use --force to overwrite)

Source layout is path-mirror: files under templates/commons/** sync to
every consumer, files under templates/<stack>/** sync to consumers of
that stack. Destination path = source path with the prefix stripped.
To add a new managed file, drop it where you want it to land — no code
edit required.

Shell→Python migration (docs/proposals/shell-to-python.md): the find/cp/cmp
subtree walk moves into Python (stdlib filecmp/shutil); kind detection comes
from release_core.manifest.detect_kind (byte-equal to bin/detect-kind). The
per-file lines (created/ok/updated/conflict), the summary line, and the
conflict→nonzero exit are preserved byte-for-byte.
"""

from __future__ import annotations

import filecmp
import os
import shutil
import sys
from dataclasses import dataclass

from .. import cli, manifest, proc

USAGE = __doc__ or ""


def _usage_block() -> str:
    """The bash `--help` block (lines 2..first-blank, `# ` stripped)."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


@dataclass
class Tally:
    created: int = 0
    updated: int = 0
    skipped: int = 0
    conflicts: int = 0


def _files_under(prefix: str) -> list[str]:
    """Absolute paths of regular files under `prefix`, excluding .DS_Store.

    The bash used `find -type f` (filesystem order, undefined); we sort for a
    deterministic, testable ordering — the only output-order change, and the
    prior order was never guaranteed.
    """
    out: list[str] = []
    for root, _dirs, files in os.walk(prefix):
        for name in files:
            if name == ".DS_Store":
                continue
            out.append(os.path.join(root, name))
    return sorted(out)


def process_subtree(prefix: str, *, force: bool, tally: Tally, emit) -> None:
    """Copy each file under `prefix` into the cwd, mirroring the bash semantics.

    `dest` = path under `prefix` with the prefix stripped. Missing → create;
    identical → ok; differing+force → update; differing → conflict. `emit` is a
    sink for the per-file line (so tests can capture without stdout).
    """
    if not os.path.isdir(prefix):
        return
    for src in _files_under(prefix):
        dest = os.path.relpath(src, prefix)
        dest_dir = os.path.dirname(dest)
        if dest_dir:
            os.makedirs(dest_dir, exist_ok=True)
        if not os.path.exists(dest):
            shutil.copyfile(src, dest)
            if os.access(src, os.X_OK):
                _chmod_x(dest)
            emit(f"  created   {dest}")
            tally.created += 1
        elif filecmp.cmp(src, dest, shallow=False):
            emit(f"  ok        {dest}")
            tally.skipped += 1
        elif force:
            shutil.copyfile(src, dest)
            if os.access(src, os.X_OK):
                _chmod_x(dest)
            emit(f"  updated   {dest}")
            tally.updated += 1
        else:
            emit(f"  conflict  {dest} (differs; --force to overwrite)")
            tally.conflicts += 1


def _chmod_x(path: str) -> None:
    """Add the executable bits the bash `chmod +x` set (u+x,g+x,o+x respecting umask is moot)."""
    import stat

    mode = os.stat(path).st_mode
    os.chmod(path, mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _release_root() -> str:
    """release/ checkout root — six parents up from this verb module."""
    here = os.path.dirname(os.path.realpath(__file__))
    return os.path.normpath(os.path.join(here, "..", "..", "..", "..", "..", ".."))


def main(argv: list[str]) -> int:
    try:
        values, _ = cli.parse(
            argv,
            [
                cli.Opt("--stack", takes_value=True, default=""),
                cli.Opt("--force"),
            ],
            doc=_usage_block(),
        )
    except SystemExit as exc:
        return int(exc.code or 0)

    stack = values["stack"] or ""
    force = bool(values["force"])

    repo_root = proc.out(["git", "rev-parse", "--show-toplevel"])
    os.chdir(repo_root)

    if not stack:
        # Bash did `stack=${stack:-$(detect-kind)}`; an undetected kind makes
        # detect-kind print "could not detect kind of <pwd>" and exit 1, which
        # (under `set -e`) aborts the sweep. Reproduce that clean exit rather
        # than letting KindError surface as a traceback. cwd is repo_root here.
        try:
            stack = manifest.detect_kind(repo_root)
        except manifest.KindError as exc:
            print(str(exc), file=sys.stderr)
            return 1

    release_root = _release_root()
    commons_dir = os.path.join(release_root, "templates", "commons")
    stack_dir = os.path.join(release_root, "templates", stack)

    if not os.path.isdir(stack_dir):
        print(f"no templates for stack '{stack}' at {stack_dir}", file=sys.stderr)
        return 1

    print(f"repo:  {os.path.basename(repo_root)}")
    print(f"stack: {stack}")
    print()

    tally = Tally()
    emit = print  # per-file lines go to stdout

    # Commons first, stack second — stack-specific files override commons on
    # collision (a stack may specialize a shared file).
    process_subtree(commons_dir, force=force, tally=tally, emit=emit)
    process_subtree(stack_dir, force=force, tally=tally, emit=emit)

    print()
    print(
        f"summary: {tally.created} created, {tally.updated} updated, "
        f"{tally.skipped} ok, {tally.conflicts} conflicts"
    )

    return 0 if tally.conflicts == 0 else 1
