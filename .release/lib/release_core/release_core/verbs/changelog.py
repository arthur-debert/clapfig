"""changelog — the bin/changelog-* family (shell→Python migration, Phase 1).

One module for the tight changelog cluster; each former bash script maps to a
``*_main`` here and is driven by its own thin shim on ``$PATH``:

  - :func:`orchestrator_main`  ← bin/changelog (the dispatch front-end)
  - :func:`add_main`           ← bin/changelog-add
  - :func:`cut_main`           ← bin/changelog-cut
  - :func:`render_main`        ← bin/changelog-render

The CLI contract is consumed by consumers + CI (changelog-tests.yml, the
changelog-check action, bin-internal/roll-changelog.sh shelling to bin/changelog)
so stdout, exit codes, flags, and the generated CHANGELOG.md / fragment bytes
match the old bash byte-for-byte. The vendored bash semver-tool is replaced for
THIS family by release_core.version; the tool's regex semantics (NAT — no
leading zeros — and NAT/ALPHANUM prerelease identifiers) are reproduced exactly
by _SEMVER_TOOL_RE so validation parity holds. (Other scripts still reference
share/semver-tool; that tree stays.)
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
from datetime import UTC, datetime

from .. import version

# --- shared helpers ---------------------------------------------------------

# Reproduces share/semver-tool's SEMVER_REGEX exactly (NAT = '0|[1-9][0-9]*',
# ALPHANUM = '[0-9]*[A-Za-z-][0-9A-Za-z-]*', IDENT = NAT|ALPHANUM), anchored.
# release_core.version.parse is laxer (accepts a leading 'v' and leading zeros),
# so we gate validity on this regex and only use version.parse for ordering.
_NAT = r"(?:0|[1-9][0-9]*)"
_ALPHANUM = r"(?:[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
_IDENT = rf"(?:{_NAT}|{_ALPHANUM})"
_SEMVER_TOOL_RE = re.compile(
    rf"^{_NAT}\.{_NAT}\.{_NAT}"
    rf"(?:-{_IDENT}(?:\.{_IDENT})*)?"
    rf"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)


def _is_valid_semver(v: str) -> bool:
    """True iff ``v`` validates the way the vendored semver-tool's `validate` did."""
    return bool(_SEMVER_TOOL_RE.match(v))


def _resolve_changelog_root() -> str | None:
    """Walk up from cwd for an existing CHANGELOG/; else fall back to git root.

    Mirrors the bash `resolve_changelog_root`: returns the first ancestor (incl.
    cwd) that contains a CHANGELOG/ dir, otherwise `git rev-parse --show-toplevel`
    (or None if that fails — not in a git repo).
    """
    d = os.getcwd()
    while d != "/":
        if os.path.isdir(os.path.join(d, "CHANGELOG")):
            return d
        d = os.path.dirname(d)
    try:
        top = subprocess.run(  # noqa: S603,S607 — fixed argv, no shell
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None
    out = top.stdout.strip()
    return out or None


def _frag_with_newline(data: bytes) -> bytes:
    """A fragment's bytes, with a single '\\n' appended iff non-empty and not
    already newline-terminated. Mirrors bash `[[ -s f && tail -c1 != "" ]]`."""
    if data and not data.endswith(b"\n"):
        return data + b"\n"
    return data


def _sorted_fragments(changelog_dir: str) -> list[str]:
    """unreleased-*.md fragment paths in stable byte order (LC_ALL=C glob)."""
    try:
        names = os.listdir(changelog_dir)
    except OSError:
        return []
    frags = [n for n in names if n.startswith("unreleased-") and n.endswith(".md")]
    frags.sort()  # byte order; ASCII filenames so codepoint sort == LC_ALL=C
    return [os.path.join(changelog_dir, n) for n in frags]


# --- changelog-add ----------------------------------------------------------

ADD_USAGE = "usage: changelog-add [--force] <slug> [body...]"

_SLUG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def add_main(argv: list[str]) -> int:
    """changelog-add [--force] <slug> [body...]"""
    root = _resolve_changelog_root()
    if not root:
        print(
            "error: no CHANGELOG/ found above cwd and not inside a git repository",
            file=sys.stderr,
        )
        return 1
    os.chdir(root)

    args = list(argv)
    force = False
    if args and args[0] == "--force":
        force = True
        args = args[1:]

    slug = args[0] if args else ""
    if not slug:
        print(ADD_USAGE, file=sys.stderr)
        return 2
    args = args[1:]

    if re.fullmatch(r"[0-9]+", slug):
        slug = f"pr-{slug}"

    if not _SLUG_RE.match(slug):
        print(
            f"error: slug must match [A-Za-z0-9][A-Za-z0-9._-]* (got: {slug})",
            file=sys.stderr,
        )
        return 2

    os.makedirs("CHANGELOG", exist_ok=True)
    target = os.path.join("CHANGELOG", f"unreleased-{slug}.md")

    if os.path.exists(target) and not force:
        print(
            f"error: {target} already exists (pass --force to overwrite)",
            file=sys.stderr,
        )
        return 1

    if args:
        # printf '%s\n' "$*": args joined by a single space, one trailing newline.
        body = (" ".join(args) + "\n").encode()
        with open(target, "wb") as fh:
            fh.write(body)
    else:
        # `cat > target`: stream stdin bytes through verbatim.
        data = sys.stdin.buffer.read()
        with open(target, "wb") as fh:
            fh.write(data)
    print(f"wrote {target}")
    return 0


# --- changelog-cut ----------------------------------------------------------

CUT_USAGE = "usage: changelog-cut <version>"


def cut_main(argv: list[str]) -> int:
    """changelog-cut <version>"""
    ver = argv[0] if argv else ""
    if not ver:
        print(CUT_USAGE, file=sys.stderr)
        return 2

    if ver[:1] in ("v", "V"):
        print(
            f"error: version must be bare semver without 'v' prefix (got: {ver})",
            file=sys.stderr,
        )
        return 2

    if not _is_valid_semver(ver):
        print(
            f"error: version must be valid semver (got: {ver})",
            file=sys.stderr,
        )
        return 2

    root = _resolve_changelog_root()
    if not root:
        print(
            "error: no CHANGELOG/ found above cwd and not inside a git repository",
            file=sys.stderr,
        )
        return 1
    os.chdir(root)

    if not os.path.isdir("CHANGELOG"):
        print("error: CHANGELOG/ directory not found", file=sys.stderr)
        return 1

    fragments = _sorted_fragments("CHANGELOG")
    if not fragments:
        print(
            "error: no CHANGELOG/unreleased-*.md fragments to cut",
            file=sys.stderr,
        )
        return 1

    target = os.path.join("CHANGELOG", f"{ver}.md")
    if os.path.exists(target):
        print(
            f"error: {target} already exists; refuse to overwrite an existing version file",
            file=sys.stderr,
        )
        return 1

    today = datetime.now(UTC).strftime("%Y-%m-%d")

    buf = bytearray()
    buf += f"## {ver} - {today}\n\n".encode()
    for f in fragments:
        with open(f, "rb") as fh:
            buf += _frag_with_newline(fh.read())
    with open(target, "wb") as fh:
        fh.write(buf)

    for f in fragments:
        os.remove(f)

    n = len(fragments)
    print(f"cut {target} ({n} fragment(s))")
    return 0


# --- changelog-render -------------------------------------------------------


def render_main(argv: list[str]) -> int:
    """changelog-render — regenerate CHANGELOG.md from CHANGELOG/*."""
    root = _resolve_changelog_root()
    if not root:
        print(
            "error: no CHANGELOG/ found above cwd and not inside a git repository",
            file=sys.stderr,
        )
        return 1
    os.chdir(root)

    if not os.path.isdir("CHANGELOG"):
        print("error: CHANGELOG/ directory not found", file=sys.stderr)
        return 1

    # Validate every CHANGELOG/<stem>.md version filename; collect the good ones.
    bad: list[str] = []
    versions: list[str] = []
    for name in sorted(os.listdir("CHANGELOG")):
        if not name.endswith(".md"):
            continue
        stem = name[: -len(".md")]
        if stem in ("README", "legacy"):
            continue
        if stem.startswith("unreleased-"):
            continue
        if stem[:1] in ("v", "V"):
            bad.append(name)
            continue
        if _is_valid_semver(stem):
            versions.append(stem)
        else:
            bad.append(name)

    if bad:
        print(
            f"error: unparseable version filename(s) in CHANGELOG/: {' '.join(bad)}",
            file=sys.stderr,
        )
        return 1

    versions_output = _sort_versions(versions)

    unreleased = _sorted_fragments("CHANGELOG")

    buf = bytearray()
    buf += b"<!-- generated - do not edit. See CHANGELOG/README.txt -->\n\n"
    buf += b"# Changelog\n\n"
    buf += b"## Unreleased\n\n"
    if unreleased:
        for f in unreleased:
            with open(f, "rb") as fh:
                buf += _frag_with_newline(fh.read())
        buf += b"\n"

    for v in versions_output:
        with open(os.path.join("CHANGELOG", f"{v}.md"), "rb") as fh:
            buf += fh.read()
        buf += b"\n"

    legacy = os.path.join("CHANGELOG", "legacy.md")
    if os.path.isfile(legacy):
        with open(legacy, "rb") as fh:
            buf += fh.read()

    # Atomic write: temp file in the same dir, then rename (matches mktemp+mv).
    target = "CHANGELOG.md"
    fd, tmp = tempfile.mkstemp(prefix="CHANGELOG.md.tmp.", dir=".")
    try:
        # mkstemp creates the temp file 0o600 and os.replace preserves that;
        # the bash `mktemp + mv` produced a umask-default (typically 0o644)
        # CHANGELOG.md. Re-derive the umask-respecting mode so the rendered
        # file stays world-readable for downstream tools/CI.
        try:
            umask = os.umask(0)
            os.umask(umask)
            os.chmod(tmp, 0o666 & ~umask)
        except OSError:
            pass
        with os.fdopen(fd, "wb") as fh:
            fh.write(buf)
        os.replace(tmp, target)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise

    print(f"rendered {target}")
    return 0


def _sort_versions(versions: list[str]) -> list[str]:
    """Descending semver order, bare release ABOVE its prereleases (semver §11).

    Replaces the bash `sort -V -r | awk` two-step. release_core.version.SemVer
    orders natively per §11 (release > prerelease; numeric identifiers rank below
    alphanumeric); reverse-sorting the parsed versions yields the same sequence
    the bash produced, but the bash GROUPS by base version then emits bare-first
    within a group. SemVer's native descending order already places a release
    above all of its own prereleases, and orders distinct base versions
    correctly, so a single reverse sort is equivalent.
    """
    return sorted(versions, key=version.parse, reverse=True)


# --- changelog (orchestrator) -----------------------------------------------

ORCHESTRATOR_USAGE = """usage: changelog <command> [args...]

Commands:
  add [--force] <slug> [body...]   add an unreleased fragment
  cut <version>                    cut unreleased fragments into a version file
  render                           regenerate CHANGELOG.md
  new-version <version>            cut + render

See CHANGELOG/README.txt and docs/proposals/changelog-handling.md."""


def orchestrator_main(argv: list[str]) -> int:
    """changelog <command> [args...] — dispatch to the add/cut/render verbs."""
    cmd = argv[0] if argv else ""
    rest = argv[1:]

    if cmd == "add":
        return add_main(rest)
    if cmd == "cut":
        return cut_main(rest)
    if cmd == "render":
        return render_main([])
    if cmd == "new-version":
        if not rest:
            print("usage: changelog new-version <version>", file=sys.stderr)
            return 2
        rc = cut_main([rest[0]])
        if rc != 0:
            return rc
        return render_main([])
    if cmd in ("-h", "--help", "help"):
        print(ORCHESTRATOR_USAGE)
        return 0
    if cmd == "":
        print(ORCHESTRATOR_USAGE, file=sys.stderr)
        return 2
    print(f"unknown command: {cmd}", file=sys.stderr)
    print(ORCHESTRATOR_USAGE, file=sys.stderr)
    return 2
