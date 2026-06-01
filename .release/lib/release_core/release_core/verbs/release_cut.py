"""release-cut — canonical "cut a release" CLI across every Kind in the
arthur-debert/* + lex-fmt/* fleets.

Everything that actually mutates state runs in CI. This script does
the local-side pre-flight:
  1. Detect the Consumer's Kind (via release_core.manifest.detect_kind).
  2. Read the current version from that Kind's canonical source
     (Cargo.toml, package.json, extension.toml, git tag, …).
  3. Compute the new version (bump shortcut or literal X.Y.Z).
  4. Dispatch .github/workflows/release.yml with the new version.

Usage:
  release-cut <new-version>          # e.g. release-cut 1.8.0
  release-cut <bump-level>           # e.g. release-cut minor
                                     #   (one of: major, minor, patch)

Pre-releases: pass a semver pre-release suffix to cut an RC or beta,
e.g. `release-cut 1.8.0-rc.1`. CI marks the GitHub Release as
"pre-release" (so it isn't shown as Latest) and skips rolling
`## [Unreleased]` in CHANGELOG.md, so subsequent RCs / the final
release can still draw from the same unreleased entries.

What CI does is per-Kind (see your repo's
`.github/workflows/release.yml` thin caller of one of
`arthur-debert/release`'s reusable release workflows: `rust-cli.yml`,
`tauri-app.yml`, `electron-app.yml`, etc.). At minimum CI bumps
version files (where applicable), rolls CHANGELOG, commits + tags,
builds, and creates the GitHub Release. Per-Kind add-ons (crates.io
publish, Homebrew formula push, .deb packages, codesigning,
Marketplace upload, …) are wired in the workflow.

Current-version source by Kind:
  rust-cli, rust-lib       Cargo.toml [workspace.package].version
                           (falls back to first workspace member —
                            covers workspace-only roots like dodot's)
  tauri-app                src-tauri/Cargo.toml [package].version
  electron-app             package.json .version
  vscode-ext               package.json .version
  tree-sitter              package.json .version
  zed-extension            extension.toml version = "..."
  nvim-plugin, go-cli      git describe --tags --abbrev=0
                           (no manifest by design — tag IS the version;
                            pass an explicit X.Y.Z for the first release)

Preconditions (checked by CI before mutating anything):
  - version is MAJOR.MINOR.PATCH[-PRERELEASE]
  - tag vX.Y.Z does not already exist
  - version differs from the manifest's current version
  - CHANGELOG.md has entries under `## [Unreleased]`

After dispatch:
  gh run watch
  gh run list --workflow=release.yml --limit=1

Shell→Python migration (docs/proposals/shell-to-python.md): the kind-aware
version readers + semver math moved here; bin/release-cut is a thin shim.
release-cut is release-only (a real file in bin/, NOT synced to consumers);
the distributed templates/commons/bin/release shim execs whatever release-cut
is on $PATH. Stdout, exit codes, and the `gh workflow run` invocation are
preserved byte-for-byte — they are pinned by tests/release-cut/release-cut.bats.
"""

from __future__ import annotations

import os
import re
import shutil
import sys

from .. import gh, manifest, proc, version
from .changelog import _SEMVER_TOOL_RE

USAGE = """\
usage: release-cut <major|minor|patch|X.Y.Z[-PRERELEASE]>

Bump shortcuts operate on the current MAJOR.MINOR.PATCH (any
pre-release suffix is stripped before bumping; to step from
1.0.0-rc.1 to 1.0.0, type the version literally).

The current version is read from the canonical source for the
Consumer's Kind (Cargo.toml, package.json, extension.toml, or the
latest git tag for Kinds without a manifest). See the script header
for the per-Kind mapping.
"""

# ---------------------------------------------------------------------
# Per-Kind current-version readers.
# Each reader returns the current version (no leading 'v') or None if it
# can't determine one — mirroring the bash readers' "print or return 1".
# ---------------------------------------------------------------------


def _read_toml_version(path: str) -> str | None:
    """Read the package version from a TOML file. Works for Cargo.toml
    ([package] / [workspace.package]) and extension.toml (top-level
    version key).

    Section-aware: starts in "matching" mode so top-level keys before
    any [section] header are read (extension.toml shape); turns off on
    any [section] header; turns back on only for [package] and
    [workspace.package]. This avoids matching a `version = "..."` line
    inside [dependencies], [workspace.dependencies.<dep>], [lints],
    [features], etc.
    """
    if not os.path.isfile(path):
        return None
    in_sect = True
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for raw in fh:
                line = raw.rstrip("\n")
                if re.match(r"^\[package\][ \t]*$", line):
                    in_sect = True
                    continue
                if re.match(r"^\[workspace\.package\][ \t]*$", line):
                    in_sect = True
                    continue
                if line.startswith("["):
                    in_sect = False
                    continue
                if in_sect and re.match(r"^[ \t]*version[ \t]*=", line):
                    m = re.search(r'"([^"]+)"', line)
                    if m:
                        return m.group(1)
    except OSError:
        return None
    return None


def _read_json_version(path: str) -> str | None:
    """Read the first top-level `"version": "..."` line from a JSON file.

    The anchor `^\\s*"version"` excludes dependency-map entries like
    `"@scope/pkg": "^1.2.3"` and `"version-helper": "..."` keys nested
    inside other objects (where the field name precedes `version`).
    """
    if not os.path.isfile(path):
        return None
    pat = re.compile(r'^[ \t]*"version"[ \t]*:[ \t]*"([^"]+)"')
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                m = pat.match(line)
                if m:
                    return m.group(1)
    except OSError:
        return None
    return None


def _read_rust_version() -> str | None:
    """Rust: try the root Cargo.toml first (single crate OR workspace with
    [workspace.package].version — the canonical layout for ~all rust
    consumers). If empty, probe workspace members for the first one
    carrying a literal version (covers dodot's workspace-only root,
    where the lib crate at crates/dodot-lib holds the canonical version
    and the bin crate inherits via version.workspace = true).
    """
    v = _read_toml_version("Cargo.toml")
    if v is not None:
        return v
    members = _workspace_members("Cargo.toml")
    if not members:
        return None
    for path in members:
        # Word-split for glob expansion ("crates/*" → multiple paths).
        for expanded in _expand_glob(path):
            v = _read_toml_version(os.path.join(expanded, "Cargo.toml"))
            if v is not None:
                return v
    return None


def _workspace_members(cargo_toml: str) -> list[str]:
    """Extract paths from the `members = [...]` array. Handles single-line
    and multi-line forms, plus shell globs (e.g. "crates/*")."""
    if not os.path.isfile(cargo_toml):
        return []
    collected: list[str] = []
    in_arr = False
    try:
        with open(cargo_toml, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if re.match(r"^[ \t]*members[ \t]*=[ \t]*\[", line):
                    in_arr = True
                if in_arr:
                    collected += re.findall(r'"([^"]+)"', line)
                    if "]" in line:
                        in_arr = False
    except OSError:
        return []
    return collected


def _expand_glob(path: str) -> list[str]:
    """Expand a members-array entry the way the bash `for expanded in $path`
    word-split + glob did. A glob with no matches expands to itself (bash
    nullglob is off), so the caller still attempts the literal path."""
    import glob as _glob

    if any(ch in path for ch in "*?["):
        matches = sorted(_glob.glob(path))
        return matches if matches else [path]
    return [path]


def _read_git_tag_version() -> str | None:
    """Kinds without a local manifest: the latest git tag is the version
    source of truth."""
    res = proc.run(["git", "describe", "--tags", "--abbrev=0"], check=False)
    if res.returncode != 0:
        return None
    tag = res.stdout.strip()
    if not tag:
        return None
    return tag[1:] if tag.startswith("v") else tag


def _read_current_version(kind: str) -> str | None:
    if kind in ("rust-cli", "rust-lib"):
        return _read_rust_version()
    if kind == "tauri-app":
        return _read_toml_version("src-tauri/Cargo.toml")
    if kind in ("electron-app", "vscode-ext", "tree-sitter"):
        return _read_json_version("package.json")
    if kind == "zed-extension":
        return _read_toml_version("extension.toml")
    if kind in ("nvim-plugin", "go-cli"):
        return _read_git_tag_version()
    print(
        f"release-cut: don't know how to read current version for Kind={kind}",
        file=sys.stderr,
    )
    return None


def _is_valid_literal_version(arg: str) -> bool:
    """Mirror the bash literal-version guard byte-for-byte.

    The bash rejected the literal unless ALL of:
      - it did not start with ``[vV]``                         (`^[vV]` test)
      - the vendored semver-tool's ``validate`` said "valid"   (SEMVER_REGEX)
      - the vendored semver-tool's ``get build`` was empty     (no ``+BUILD``)

    semver-tool's SEMVER_REGEX uses NAT = ``0|[1-9][0-9]*``, so it REJECTS
    leading-zero numeric fields (``01.0.0``, ``1.00.0``, ``1.0.0-01``) that
    ``release_core.version.parse`` would silently ACCEPT (and normalize). We
    therefore gate validity on the strict ``_SEMVER_TOOL_RE`` (shared with the
    changelog migration, which reproduces that exact regex) rather than on
    ``version.parse``. ``version.parse``/``version.bump`` stay in charge of the
    bump-shortcut computation; only this VALIDATION gate is strict.

    ``_SEMVER_TOOL_RE`` itself permits a trailing ``+BUILD`` (the regex does),
    so we keep the explicit ``+`` rejection in front of it to reproduce the
    bash ``get build`` guard. The leading-``v`` rejection reproduces ``^[vV]``.
    """
    if arg[:1] in ("v", "V"):
        return False
    if "+" in arg:  # the bash checked `semver get build` was empty
        return False
    return bool(_SEMVER_TOOL_RE.match(arg))


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash control flow
    if len(argv) != 1:
        print(USAGE, file=sys.stderr, end="")
        return 2
    arg = argv[0]
    if arg in ("-h", "--help"):
        print(USAGE, file=sys.stderr, end="")
        return 0

    try:
        repo_root = proc.out(["git", "rev-parse", "--show-toplevel"])
    except proc.ProcError as exc:
        print(exc.stderr.strip(), file=sys.stderr)
        return 1
    os.chdir(repo_root)

    # Graceful no-op for Consumers that don't ship a release workflow
    # (github-action repos, tree-sitter standalone grammars, etc.).
    if not os.path.isfile(".github/workflows/release.yml"):
        print("release-cut: no .github/workflows/release.yml in this repo; nothing to do.")
        return 0

    if arg in ("major", "minor", "patch"):
        # Detect Kind lazily — only needed for bump shortcuts that read the
        # current version. Literal-version dispatches skip this so they work
        # in any Consumer regardless of Kind.
        try:
            kind = manifest.detect_kind(".")
        except manifest.KindError:
            print(
                "release-cut: detect-kind could not identify this repo's Kind",
                file=sys.stderr,
            )
            print(
                "  (required for bump shortcuts; pass an explicit X.Y.Z to bypass)",
                file=sys.stderr,
            )
            return 1
        current = _read_current_version(kind)
        if current is None:
            if kind in ("nvim-plugin", "go-cli"):
                print(
                    f"release-cut: no git tags found — Kind={kind} reads the current version\n"
                    "  from `git describe --tags --abbrev=0`. Pass an explicit version\n"
                    "  (e.g. release-cut 0.1.0) for the first release.",
                    file=sys.stderr,
                )
            else:
                print(
                    f"release-cut: couldn't determine current version for Kind={kind}.\n"
                    "  Expected manifest not found or has no version field.",
                    file=sys.stderr,
                )
            return 1
        new_version = version.fmt(version.bump(version.parse(current), arg))
        print(f"Bumping {arg}: {current} -> {new_version}")
    elif _is_valid_literal_version(arg):
        new_version = arg
    else:
        print(
            "release-cut: version must be MAJOR.MINOR.PATCH[-PRERELEASE] or one of: "
            f"major, minor, patch (got: {arg})",
            file=sys.stderr,
        )
        return 2

    if shutil.which("gh") is None:
        print(
            "release-cut: gh CLI not found — install from https://cli.github.com/",
            file=sys.stderr,
        )
        return 1

    print(f"Triggering release.yml for v{new_version}...")
    # proc.run captures; forward gh's own stdout/stderr so the dispatch output
    # (and any gh error) reaches the user exactly as the bash `gh ...` did.
    res = gh.workflow_run("release.yml", fields={"version": new_version})
    if res.stdout:
        sys.stdout.write(res.stdout)
    if res.stderr:
        sys.stderr.write(res.stderr)
    if res.returncode != 0:
        return res.returncode

    print(
        "\nWorkflow queued. Follow with:\n"
        "  gh run watch\n"
        "  gh run list --workflow=release.yml --limit=1"
    )
    return 0
