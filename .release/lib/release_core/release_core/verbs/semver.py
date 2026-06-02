"""semver — the validate/get edge of the vendored bash semver-tool.

Drop-in replacement for the operations the release pipeline's prepare-* callers
shell out to (bin-internal/prepare-{nvim,tauri}-release.sh, the
prepare-release-{npm,python,go} and prepare-release composite actions, and the
gh-action.yml reusable workflow that runs from a consumer's synced bin/):

  - ``semver validate <version>``                  → 'valid'/'invalid', exit 0
  - ``semver get major|minor|patch <version>``     → the numeric component
  - ``semver get prerel|prerelease <version>``     → PRERELEASE part (no '-')
  - ``semver get build <version>``                 → BUILD part (no '+')
  - ``semver get release <version>``               → 'MAJOR.MINOR.PATCH'

This is intentionally a SUBSET of the vendored tool's surface — it implements
``validate`` and ``get`` (every ``get`` part the callers reach), but NOT
``bump``/``compare``/``diff``. The output and exit codes are byte-compatible
with templates/commons/bin/share/semver-tool/semver for these operations:

  - ``validate`` matches the vendored SEMVER_REGEX (a leading 'v'/'V' is
    accepted; numeric identifiers must NOT have leading zeros), printing
    'valid'/'invalid' and exiting 0 either way.
  - ``get`` first validates the version (error to stderr + exit 1 on a
    malformed version, mirroring the vendored ``validate_version`` ``error``),
    then prints the requested part: the prerel/build parts have their ``-``/
    ``+`` marker stripped, and ``release`` is the bare 'MAJOR.MINOR.PATCH' with
    any leading 'v' dropped.

Validation reproduces share/semver-tool's SEMVER_REGEX (the same NAT/IDENT
semantics changelog.py's _SEMVER_TOOL_RE pins: NAT = no leading zeros,
IDENT = NAT|ALPHANUM), extended to accept the optional leading 'v'/'V' and to
capture the component groups the ``get`` subcommand returns.
"""

from __future__ import annotations

import re
import sys

# Reproduces share/semver-tool's SEMVER_REGEX exactly, including the optional
# leading [vV] the vendored tool accepted. NAT = '0|[1-9][0-9]*' (no leading
# zeros), ALPHANUM = '[0-9]*[A-Za-z-][0-9A-Za-z-]*', IDENT = NAT|ALPHANUM,
# FIELD = '[0-9A-Za-z-]+' (build identifiers, no leading-zero rule).
_NAT = r"(?:0|[1-9][0-9]*)"
_ALPHANUM = r"(?:[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
_IDENT = rf"(?:{_NAT}|{_ALPHANUM})"
_FIELD = r"(?:[0-9A-Za-z-]+)"
_SEMVER_TOOL_RE = re.compile(
    rf"^[vV]?"
    rf"(?P<major>{_NAT})\.(?P<minor>{_NAT})\.(?P<patch>{_NAT})"
    rf"(?:-(?P<prerel>{_IDENT}(?:\.{_IDENT})*))?"
    rf"(?:\+(?P<build>{_FIELD}(?:\.{_FIELD})*))?$"
)

_GET_PARTS = ("major", "minor", "patch", "prerel", "prerelease", "build", "release")

USAGE = "usage: semver validate <version> | semver get <part> <version>"


def _match(version: str) -> re.Match[str] | None:
    return _SEMVER_TOOL_RE.match(version)


def _error(msg: str) -> int:
    """Mirror the vendored tool's ``error``: message to stderr, exit 1."""
    print(msg, file=sys.stderr)
    return 1


def _command_validate(argv: list[str]) -> int:
    if len(argv) != 1:
        return _error(USAGE)
    print("valid" if _match(argv[0]) else "invalid")
    return 0


def _command_get(argv: list[str]) -> int:
    if len(argv) != 2 or not argv[0] or not argv[1]:
        return _error(USAGE)
    part, version = argv[0], argv[1]
    if part not in _GET_PARTS:
        return _error(USAGE)

    m = _match(version)
    if not m:
        # Byte-for-byte with the vendored validate_version's error message.
        return _error(
            f"version {version} does not match the semver scheme "
            "'X.Y.Z(-PRERELEASE)(+BUILD)'. See help for more information."
        )

    if part == "release":
        print(f"{m.group('major')}.{m.group('minor')}.{m.group('patch')}")
    elif part == "prerelease":
        print(m.group("prerel") or "")
    else:  # major | minor | patch | prerel | build
        print(m.group(part) or "")
    return 0


def main(argv: list[str]) -> int:
    if not argv:
        return _error(USAGE)
    cmd, rest = argv[0], argv[1:]
    if cmd == "validate":
        return _command_validate(rest)
    if cmd == "get":
        return _command_get(rest)
    if cmd in ("-h", "--help"):
        print(USAGE)
        return 0
    return _error(USAGE)
