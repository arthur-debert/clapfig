"""semver parse / compare / bump — the bash semver-tool replacement."""

from __future__ import annotations

import pytest
from release_core import version
from release_core.version import SemVer


def test_parse_plain():
    v = version.parse("1.2.3")
    assert (v.major, v.minor, v.patch) == (1, 2, 3)
    assert v.prerelease == ()


def test_parse_leading_v():
    v = version.parse("v2.0.1")
    assert (v.major, v.minor, v.patch) == (2, 0, 1)


def test_parse_prerelease():
    v = version.parse("1.0.0-rc.1")
    assert v.prerelease == ("rc", 1)


def test_parse_strips_build_metadata():
    v = version.parse("1.2.3+build.7")
    assert (v.major, v.minor, v.patch) == (1, 2, 3)
    assert v.prerelease == ()


def test_parse_rejects_garbage():
    with pytest.raises(ValueError):
        version.parse("not.a.version")


def test_parse_rejects_two_part():
    with pytest.raises(ValueError):
        version.parse("1.2")


@pytest.mark.parametrize(
    ("lo", "hi"),
    [
        ("1.0.0", "2.0.0"),
        ("1.0.0", "1.1.0"),
        ("1.0.0", "1.0.1"),
        ("1.0.0-rc.1", "1.0.0"),  # prerelease < its release
        ("1.0.0-alpha", "1.0.0-beta"),  # alphanumeric ordering
        ("1.0.0-rc.1", "1.0.0-rc.2"),  # numeric ordering
        ("1.0.0-1", "1.0.0-alpha"),  # numeric ranks below alphanumeric
    ],
)
def test_ordering(lo, hi):
    assert version.parse(lo) < version.parse(hi)


def test_equality():
    assert version.parse("v1.2.3") == version.parse("1.2.3")


def test_sorted():
    versions = [version.parse(s) for s in ("1.2.0", "1.0.0-rc.1", "1.0.0", "2.0.0")]
    assert [version.fmt(v) for v in sorted(versions)] == ["1.0.0-rc.1", "1.0.0", "1.2.0", "2.0.0"]


def test_bump_major_zeroes_rest():
    assert version.bump(version.parse("1.2.3"), "major") == version.parse("2.0.0")


def test_bump_minor_zeroes_patch():
    assert version.bump(version.parse("1.2.3"), "minor") == version.parse("1.3.0")


def test_bump_patch():
    assert version.bump(version.parse("1.2.3"), "patch") == version.parse("1.2.4")


def test_bump_strips_prerelease():
    assert version.bump(version.parse("1.2.3-rc.1"), "patch") == version.parse("1.2.4")


def test_bump_rejects_bad_part():
    with pytest.raises(ValueError):
        version.bump(version.parse("1.0.0"), "epoch")


def test_fmt_plain():
    assert version.fmt(SemVer(1, 2, 3)) == "1.2.3"


def test_fmt_prefix():
    assert version.fmt(version.parse("1.2.3"), prefix="v") == "v1.2.3"


def test_fmt_prerelease_roundtrip():
    assert version.fmt(version.parse("1.0.0-rc.2")) == "1.0.0-rc.2"


def test_str_dunder():
    assert str(version.parse("v3.4.5")) == "3.4.5"
