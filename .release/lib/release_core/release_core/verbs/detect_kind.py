"""detect-kind — detect the Kind of a directory by inspecting its filesystem.

Prints one of: rust-cli, go-cli, electron-app, tauri-app, zed-extension,
vscode-ext, nvim-plugin, tree-sitter, docs-site, static-site, brew-tap,
github-action — or exits 1 (with "could not detect kind of <dir>" on stderr)
if undetermined.

Usage: detect-kind [<dir>]   (defaults to current dir)

Canary for the shell→Python migration: the
logic lives in release_core.manifest.detect_kind; this verb is the thin CLI
edge that preserves bin/detect-kind's contract byte-for-byte.
"""

from __future__ import annotations

import os
import sys

from .. import manifest

USAGE = __doc__ or ""


def main(argv: list[str]) -> int:
    if argv and argv[0] in ("-h", "--help"):
        print(USAGE.strip())
        return 0

    # Mirror the bash: `dir=${1:-.}` then `cd "$dir"`. A nonexistent dir is the
    # same `cd` failure bash would hit (set -e → nonzero exit).
    d = argv[0] if argv else "."
    if not os.path.isdir(d):
        print(f"detect-kind: {d}: No such directory", file=sys.stderr)
        return 1

    try:
        kind = manifest.detect_kind(d)
    except manifest.KindError:
        # Byte-for-byte with `echo "could not detect kind of $(pwd)"`: bash's
        # `pwd` is LOGICAL (uses $PWD, symlinks unresolved). os.path.realpath
        # would resolve them (/var → /private/var on macOS), diverging. Build
        # the logical path the way the shell would.
        print(f"could not detect kind of {_logical_pwd(d)}", file=sys.stderr)
        return 1
    print(kind)
    return 0


def _logical_pwd(d: str) -> str:
    """The path `cd "$d"; pwd` would print — logical (symlinks unresolved)."""
    if os.path.isabs(d):
        return os.path.normpath(d)
    base = os.environ.get("PWD") or os.getcwd()
    return os.path.normpath(os.path.join(base, d))
