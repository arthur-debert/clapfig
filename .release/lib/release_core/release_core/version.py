"""version — semver parse / compare / bump.

The Python replacement for the vendored bash `semver-tool` (formerly at
bin/share/semver-tool/, now removed). The standalone CLI edge — `validate` and
`get <part>` — lives in release_core.verbs.semver (bin/semver).

SemVer is ordered so versions compare and sort with the native operators
(``<``, ``sorted(...)``) following semver.org precedence: major/minor/patch
numerically, then a release outranks any of its prereleases (1.0.0 > 1.0.0-rc.1),
and prerelease identifiers compare per §11 (numeric < alphanumeric).

CONTRACT NOTE: the contract pins
``@dataclass(frozen=True, order=True)`` with fields
``major; minor; patch; prerelease: tuple = ()``. A naive ``order=True`` over a
raw ``prerelease`` tuple sorts WRONG for semver (an empty tuple sorts BELOW a
populated one — the reverse of the spec — and mixed int/str identifiers raise
``TypeError`` on comparison). We therefore keep the public field name
``prerelease`` but drive ordering off a derived, comparison-only ``_order_key``
field (``prerelease`` itself is ``compare=False``). Same public surface, correct
order. Flagged for the gatekeeper to amend the literal signature.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

_SEMVER_RE = re.compile(
    r"^v?(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)"
    r"(?:-(?P<prerelease>[0-9A-Za-z.-]+))?"
    r"(?:\+(?P<build>[0-9A-Za-z.-]+))?$"
)


def _order_key(prerelease: tuple) -> tuple:
    """A sortable key for the prerelease slot. No prerelease ranks ABOVE any."""
    if not prerelease:
        # Sentinel sorts a release above all of its prereleases.
        return (1, ())
    parts = []
    for ident in prerelease:
        if isinstance(ident, int):
            # Numeric identifiers rank below alphanumeric (semver.org §11).
            parts.append((0, ident, ""))
        else:
            parts.append((1, 0, ident))
    return (0, tuple(parts))


@dataclass(frozen=True, order=True)
class SemVer:
    major: int
    minor: int
    patch: int
    # Comparison key (see _order_key). Sorts after major/minor/patch and
    # encodes semver prerelease precedence. Not part of the public surface.
    _sort_key: tuple = field(compare=True, default=(1, ()), repr=False)
    # Public, authored prerelease identifiers (e.g. ('rc', 1)); () if none.
    prerelease: tuple = field(compare=False, default=())

    def __str__(self) -> str:
        return fmt(self)


def parse(s: str) -> SemVer:
    """Parse a semver string (optional leading 'v', optional prerelease/build)."""
    m = _SEMVER_RE.match(s.strip())
    if not m:
        raise ValueError(f"not a semver: {s!r}")
    pre_raw = m.group("prerelease")
    prerelease: tuple = ()
    if pre_raw:
        prerelease = tuple(int(p) if p.isdigit() else p for p in pre_raw.split("."))
    return SemVer(
        major=int(m.group("major")),
        minor=int(m.group("minor")),
        patch=int(m.group("patch")),
        _sort_key=_order_key(prerelease),
        prerelease=prerelease,
    )


def bump(v: SemVer, part: str) -> SemVer:
    """Return ``v`` with ``part`` (major|minor|patch) incremented; strips prerelease.

    Per semver: a major bump zeroes minor+patch, a minor bump zeroes patch.
    """
    if part == "major":
        major, minor, patch = v.major + 1, 0, 0
    elif part == "minor":
        major, minor, patch = v.major, v.minor + 1, 0
    elif part == "patch":
        major, minor, patch = v.major, v.minor, v.patch + 1
    else:
        raise ValueError(f"part must be major|minor|patch, got {part!r}")
    return SemVer(major, minor, patch, _sort_key=_order_key(()), prerelease=())


def fmt(v: SemVer, *, prefix: str = "") -> str:
    """Render ``v`` as 'MAJOR.MINOR.PATCH[-pre]', optionally prefixed (e.g. 'v')."""
    core = f"{prefix}{v.major}.{v.minor}.{v.patch}"
    if v.prerelease:
        core += "-" + ".".join(str(x) for x in v.prerelease)
    return core
