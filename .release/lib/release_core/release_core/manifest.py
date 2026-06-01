"""manifest — Kind detection + manifest/config parsing.

:func:`detect_kind` is the Python port of ``bin/detect-kind``. Its output is
consumed by release-sync, release-cut, done-check, and BATS contract tests, so
it MUST match the bash byte-for-byte: same Kind strings, same precedence, same
"could not detect kind of <pwd>" stderr + exit semantics (here: a ValueError the
shim turns into that message + exit 1).
"""

from __future__ import annotations

import os

from . import yamlio


class KindError(ValueError):
    """detect_kind could not classify the directory (maps to detect-kind's exit 1)."""


def _has_lua_source(root: str) -> bool:
    """Mirror `find . -maxdepth 3 -name '*.lua' -print -quit | grep -q .`.

    True iff a lua/ dir exists OR any *.lua file is found within depth 3.
    """
    if os.path.isdir(os.path.join(root, "lua")):
        return True
    # `find . -maxdepth 3`: the start dir is depth 0; a file directly under it
    # is depth 1, so files match at find-depths 1, 2, 3. A file living in a
    # directory whose path-depth-below-root is `dd` is at find-depth dd+1, so we
    # only inspect filenames in directories with dd <= 2.
    root_depth = root.rstrip(os.sep).count(os.sep)
    for dirpath, _dirnames, filenames in os.walk(root):
        dir_depth = dirpath.rstrip(os.sep).count(os.sep) - root_depth
        if dir_depth > 2:
            continue
        if any(name.endswith(".lua") for name in filenames):
            return True
    return False


def _grep(path: str, needles: tuple[str, ...]) -> bool:
    """Mirror `grep -q '<needle>' <path>` (any needle present, errors → False)."""
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            content = fh.read()
    except OSError:
        return False
    return any(needle in content for needle in needles)


def detect_kind(root: str | None = None) -> str:
    """Filesystem-signal Kind detection. Matches bin/detect-kind byte-for-byte.

    Raises :class:`KindError` when undetermined (the shim renders the bash
    "could not detect kind of <pwd>" stderr line + exit 1).
    """
    d = root if root is not None else "."

    def isdir(p: str) -> bool:
        return os.path.isdir(os.path.join(d, p))

    def isfile(p: str) -> bool:
        return os.path.isfile(os.path.join(d, p))

    # brew tap
    if isdir("Formula") or isdir("Casks"):
        return "brew-tap"

    # tree-sitter grammar
    if isfile("grammar.js"):
        return "tree-sitter"

    # tauri-app — precedes rust-cli AND electron-app; src-tauri/Cargo.toml +
    # root package.json is the discriminator.
    if isfile("src-tauri/Cargo.toml") and isfile("package.json"):
        return "tauri-app"

    # zed-extension — precedes rust-cli; extension.toml + Cargo.toml.
    if isfile("extension.toml") and isfile("Cargo.toml"):
        return "zed-extension"

    # rust-cli — all rust consumers are CLIs today.
    if isfile("Cargo.toml"):
        return "rust-cli"

    # go-cli
    if isfile("go.mod"):
        return "go-cli"

    # javascript-side
    if isfile("package.json"):
        pkg = os.path.join(d, "package.json")
        if _grep(pkg, ('"electron-builder"', '"electron"')):
            return "electron-app"
        if _grep(pkg, ('"@vscode/vsce"', '"vsce"')):
            return "vscode-ext"

    # composite github action
    if isfile("action.yml") or isfile("action.yaml"):
        return "github-action"

    # nvim plugin: classic vimscript layout OR modern lua-only layout. A layout
    # marker dir is necessary but not sufficient — confirm there's actual lua
    # source (excludes a grammar-only repo, already matched by grammar.js).
    nvim_layout = any(
        isdir(name) for name in ("plugin", "ftdetect", "ftplugin", "autoload", "queries")
    )
    if nvim_layout and _has_lua_source(d):
        return "nvim-plugin"

    # docs-site (mkdocs) — precedes static-site; root mkdocs.yml is the signal.
    if isfile("mkdocs.yml"):
        return "docs-site"

    # static site (mdbook or jekyll)
    if isfile("book.toml") or isfile("_config.yml"):
        return "static-site"

    raise KindError(f"could not detect kind of {os.path.realpath(d)}")


def load_sync_config(root: str | None = None) -> dict:
    """Parse ``.release-sync.yaml`` (via yamlio) → dict. ``{}`` if absent/empty."""
    d = root if root is not None else "."
    path = os.path.join(d, ".release-sync.yaml")
    if not os.path.isfile(path):
        return {}
    data = yamlio.load(path)
    return data if isinstance(data, dict) else {}


def kind_manifest(kind: str, release_home: str) -> dict:
    """Load ``templates/<kind>/manifest.yaml``. ``{}`` if absent/empty."""
    path = os.path.join(release_home, "templates", kind, "manifest.yaml")
    if not os.path.isfile(path):
        return {}
    data = yamlio.load(path)
    return data if isinstance(data, dict) else {}
