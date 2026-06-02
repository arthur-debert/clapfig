"""semver verb — validate/get edge of the (removed) vendored bash semver-tool.

Proves byte-compatible stdout + exit codes for the THREE operations the
release pipeline's prepare-* callers shell out to: ``validate <v>``,
``get prerel <v>``, ``get build <v>``. Parity cases mirror the vendored tool's
SEMVER_REGEX semantics (leading-zero rejection, leading-v tolerance,
NAT/ALPHANUM prerelease identifiers) — the same contract test_core_version.py
and test_core_changelog.py pin.
"""

from __future__ import annotations

import pytest
from release_core.verbs import semver


def _run(capsys, argv):
    rc = semver.main(argv)
    captured = capsys.readouterr()
    return rc, captured.out, captured.err


# --- validate ---------------------------------------------------------------


@pytest.mark.parametrize(
    "version",
    [
        "1.2.3",
        "0.0.0",
        "v1.2.3",
        "V1.2.3",
        "1.2.3-rc.1",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.2.3-rc.1+build.5",
        "1.2.3+build.5",
        "10.20.30",
    ],
)
def test_validate_valid(capsys, version):
    rc, out, _ = _run(capsys, ["validate", version])
    assert rc == 0
    assert out == "valid\n"


@pytest.mark.parametrize(
    "version",
    [
        "01.2.3",  # leading zero in major (NAT rejects)
        "1.02.3",  # leading zero in minor
        "1.2.03",  # leading zero in patch
        "1.2",  # too few parts
        "1.2.3.4",  # too many parts
        "not-a-version",
        "1.2.3-rc.01",  # leading zero in numeric prerelease identifier
        "",
    ],
)
def test_validate_invalid(capsys, version):
    rc, out, _ = _run(capsys, ["validate", version])
    # The vendored tool always exits 0 for `validate`, printing valid/invalid.
    assert rc == 0
    assert out == "invalid\n"


# --- get major | minor | patch | release ------------------------------------


@pytest.mark.parametrize(
    ("part", "version", "expected"),
    [
        ("major", "1.2.3", "1"),
        ("minor", "1.2.3", "2"),
        ("patch", "1.2.3", "3"),
        ("major", "v10.20.30", "10"),  # leading v tolerated, stripped
        ("minor", "V10.20.30", "20"),
        ("major", "2.3.4-rc.1+b.5", "2"),
        ("release", "1.2.3", "1.2.3"),
        ("release", "1.2.3-rc.1+build.5", "1.2.3"),  # strips prerel + build
        ("release", "v1.2.3", "1.2.3"),  # strips leading v
    ],
)
def test_get_component(capsys, part, version, expected):
    rc, out, _ = _run(capsys, ["get", part, version])
    assert rc == 0
    assert out == expected + "\n"


# --- get prerel -------------------------------------------------------------


@pytest.mark.parametrize(
    ("version", "expected"),
    [
        ("1.2.3", ""),
        ("1.2.3-rc.1", "rc.1"),
        ("1.0.0-alpha", "alpha"),
        ("1.0.0-alpha.1", "alpha.1"),
        ("1.2.3-rc.1+build.5", "rc.1"),  # build part stripped off
        ("v1.2.3-rc.2", "rc.2"),  # leading v tolerated like the vendored tool
        ("1.2.3+build.5", ""),  # build-only → no prerelease
    ],
)
def test_get_prerel(capsys, version, expected):
    rc, out, _ = _run(capsys, ["get", "prerel", version])
    assert rc == 0
    assert out == expected + "\n"


def test_get_prerelease_synonym(capsys):
    rc, out, _ = _run(capsys, ["get", "prerelease", "1.2.3-rc.1"])
    assert rc == 0
    assert out == "rc.1\n"


# --- get build --------------------------------------------------------------


@pytest.mark.parametrize(
    ("version", "expected"),
    [
        ("1.2.3", ""),
        ("1.2.3+build.5", "build.5"),
        ("1.2.3-rc.1+build.5", "build.5"),
        ("1.2.3-rc.1", ""),  # prerelease-only → no build
        ("v1.2.3", ""),  # leading v, no build
    ],
)
def test_get_build(capsys, version, expected):
    rc, out, _ = _run(capsys, ["get", "build", version])
    assert rc == 0
    assert out == expected + "\n"


# --- get on a malformed version: error to stderr, exit 1 --------------------


@pytest.mark.parametrize("part", ["prerel", "build", "prerelease"])
def test_get_invalid_version_errors(capsys, part):
    rc, out, err = _run(capsys, ["get", part, "01.2.3"])
    assert rc == 1
    assert out == ""
    # Byte-for-byte with the vendored validate_version error message.
    assert err == (
        "version 01.2.3 does not match the semver scheme "
        "'X.Y.Z(-PRERELEASE)(+BUILD)'. See help for more information.\n"
    )


# --- usage / dispatch errors ------------------------------------------------


def test_no_args_usage_error(capsys):
    rc, _, err = _run(capsys, [])
    assert rc == 1
    assert err.startswith("usage: semver")


def test_unknown_command_usage_error(capsys):
    rc, _, err = _run(capsys, ["bump", "major", "1.2.3"])
    assert rc == 1
    assert err.startswith("usage: semver")


def test_get_unknown_part_usage_error(capsys):
    rc, _, err = _run(capsys, ["get", "bogus", "1.2.3"])
    assert rc == 1
    assert err.startswith("usage: semver")


def test_validate_wrong_arity_usage_error(capsys):
    rc, _, err = _run(capsys, ["validate", "1.2.3", "extra"])
    assert rc == 1
    assert err.startswith("usage: semver")


def test_help_flag(capsys):
    rc, out, _ = _run(capsys, ["--help"])
    assert rc == 0
    assert out.startswith("usage: semver")
