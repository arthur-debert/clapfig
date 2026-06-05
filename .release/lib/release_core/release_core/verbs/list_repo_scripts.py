"""list-repo-scripts — for each managed repo, list the contents of its
bin/, scripts/, and app-bin/ directories in side-by-side columns.

The repo list is the canonical "managed by arthur-debert/release" set
(NOT auto-discovered from the main-branch-protection ruleset — that
signal includes unrelated repos). Update REPOS below when the
managed set changes.

Usage:
  list-repo-scripts                 # all managed repos, all dirs
  list-repo-scripts --only-present  # skip repos missing locally or without any dir
  list-repo-scripts --owner lex-fmt # filter by owner prefix
  list-repo-scripts --only-bin      # show only bin/ column
  list-repo-scripts --only-scripts  # show only scripts/ column
  list-repo-scripts --only-app-bin  # show only app-bin/ column
  list-repo-scripts --only-bin --only-scripts  # composable

Shell→Python migration: the mktemp/sed/wc
column-printing machinery moved into Python. The columnar output (30-col width,
headers, rules, the per-dir totals footer) is preserved byte-for-byte. Filesystem
only — no gh, no network.
"""

from __future__ import annotations

import os
import sys

USAGE = __doc__ or ""

# repo|local-path. Keep aligned with
# ~/.claude/projects/-Users-adebert-h-release/memory/project_managed_repos.md
REPOS = [
    ("arthur-debert/release", "{home}/h/release"),
    ("phos-editor/app", "{home}/h/phos/phos-app"),
    ("phos-editor/core", "{home}/h/phos/phos-core"),
    ("arthur-debert/burgertocow", "{home}/h/burgertocow"),
    ("arthur-debert/clapfig", "{home}/h/clapfig"),
    ("arthur-debert/dodot", "{home}/h/dodot"),
    ("arthur-debert/homebrew-tools", "{home}/h/homebrew-tools"),
    ("arthur-debert/padz", "{home}/h/padz"),
    ("arthur-debert/rustloc", "{home}/h/rustloc"),
    ("arthur-debert/simple-gal", "{home}/h/simple-gal/simple-gal"),
    ("arthur-debert/simple-gal-action", "{home}/h/simple-gal/simple-gal-action"),
    ("arthur-debert/simple-gal-ui", "{home}/h/simple-gal/simple-gal-ui"),
    ("arthur-debert/standout", "{home}/h/standout"),
    ("lex-fmt/comms", "{home}/h/lex-fmt/comms"),
    ("lex-fmt/lex", "{home}/h/lex-fmt/lex"),
    ("lex-fmt/lexed", "{home}/h/lex-fmt/lexed"),
    ("lex-fmt/nvim", "{home}/h/lex-fmt/nvim"),
    ("lex-fmt/tree-sitter-lex", "{home}/h/lex-fmt/tree-sitter-lex"),
    ("lex-fmt/vscode", "{home}/h/lex-fmt/vscode"),
    ("lex-fmt/zed-lex", "{home}/h/lex-fmt/zed-lex"),
]

COL = 30


def _repos() -> list[tuple[str, str]]:
    home = os.path.expanduser("~")
    return [(repo, path.format(home=home)) for repo, path in REPOS]


def _count_real_files(dir_path: str) -> int:
    """Count entries that exist OR are symlinks (a broken symlink fails -e but
    still counts — matches the bash `[ -e ] || [ -L ]`)."""
    if not os.path.isdir(dir_path):
        return 0
    count = 0
    for name in os.listdir(dir_path):
        full = os.path.join(dir_path, name)
        if os.path.exists(full) or os.path.islink(full):
            count += 1
    return count


def _col_lines(dir_path: str) -> list[str]:
    """The lines for one column: the dir's `ls -1` (sorted), or a placeholder."""
    if not os.path.isdir(dir_path):
        return ["(not present)"]
    contents = sorted(os.listdir(dir_path))
    if not contents:
        return ["(empty)"]
    return contents


def _print_columns(columns: list[list[str]], active_dirs: list[str]) -> None:
    ncols = len(active_dirs)
    max_rows = max((len(c) for c in columns), default=0)
    sep = "-" * (COL - 2)

    # header + rule
    hdr_parts: list[str] = []
    rule_parts: list[str] = []
    for i, d in enumerate(active_dirs):
        if i < ncols - 1:
            hdr_parts.append(f"{d + '/':<{COL}} ")
            rule_parts.append(f"{sep:<{COL}} ")
        else:
            hdr_parts.append(f"{d}/")
            rule_parts.append(sep)
    print("".join(hdr_parts))
    print("".join(rule_parts))

    for row in range(max_rows):
        line_parts: list[str] = []
        for i, col in enumerate(columns):
            cell = col[row] if row < len(col) else ""
            if i < ncols - 1:
                line_parts.append(f"{cell:<{COL}} ")
            else:
                line_parts.append(cell)
        print("".join(line_parts))


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch + render mirrors the bash
    only_present = False
    owner_filter = ""
    show_bin = False
    show_scripts = False
    show_app_bin = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--only-present":
            only_present = True
        elif arg == "--owner":
            i += 1
            owner_filter = argv[i] if i < len(argv) else ""
        elif arg == "--only-bin":
            show_bin = True
        elif arg == "--only-scripts":
            show_scripts = True
        elif arg == "--only-app-bin":
            show_app_bin = True
        elif arg in ("-h", "--help"):
            print(USAGE.strip().split("\n\nShell→Python")[0])
            return 0
        else:
            print(f"unknown arg: {arg}", file=sys.stderr)
            return 64
        i += 1

    if not (show_bin or show_scripts or show_app_bin):
        show_bin = show_scripts = show_app_bin = True

    active_dirs: list[str] = []
    if show_bin:
        active_dirs.append("bin")
    if show_scripts:
        active_dirs.append("scripts")
    if show_app_bin:
        active_dirs.append("app-bin")

    total_bin = total_scripts = total_app_bin = 0
    first = True

    for repo, path in _repos():
        if owner_filter and repo.split("/", 1)[0] != owner_filter:
            continue

        if not os.path.isdir(path):
            if only_present:
                continue
            if not first:
                print()
            first = False
            print(f"=== {repo} ===")
            print("(repo not cloned locally)")
            continue

        has_any = any(os.path.isdir(os.path.join(path, d)) for d in active_dirs)
        if not has_any and only_present:
            continue

        # accumulate totals (always count all three for summary)
        total_bin += _count_real_files(os.path.join(path, "bin"))
        total_scripts += _count_real_files(os.path.join(path, "scripts"))
        total_app_bin += _count_real_files(os.path.join(path, "app-bin"))

        if not first:
            print()
        first = False
        print(f"=== {repo} ===")

        columns = [_col_lines(os.path.join(path, d)) for d in active_dirs]
        _print_columns(columns, active_dirs)

    print()
    print("--- totals across fleet ---")
    if show_bin:
        print(f"  bin/     {total_bin}")
    if show_scripts:
        print(f"  scripts/ {total_scripts}")
    if show_app_bin:
        print(f"  app-bin/ {total_app_bin}")
    return 0
