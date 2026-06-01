"""release_lex verb — pure decision logic + arg parsing + sequencing helpers.

The live multi-repo orchestration (fetch/checkout/pull/commit/push/reset/
submodule, gh pr create/merge, gh run list/watch) is genuine side-effecting
glue requiring real repos + GitHub, so it is NOT unit-tested (that is the
script's whole point). Tested here: the github-slug map, compute_new_version,
the bump-kind / version recognizers, the --only filter, the PR-number and
run-id extractors, the status-line renderer, and the arg-parse + validation
exit codes (data layer mocked via tmp dirs — nothing is ever pushed/merged)."""

from __future__ import annotations

import os
import stat

import pytest
from release_core.verbs import release_lex as rlx

# --------------------------------------------------------------------------
# github_slug_for
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    "key,slug",
    [
        ("comms", "lex-fmt/comms"),
        ("lex", "lex-fmt/lex"),
        ("tree-sitter", "lex-fmt/tree-sitter-lex"),
        ("vscode", "lex-fmt/vscode"),
        ("nvim", "lex-fmt/nvim"),
        ("lexed", "lex-fmt/lexed"),
    ],
)
def test_github_slug_known(key, slug):
    assert rlx.github_slug_for(key) == slug


def test_github_slug_unknown_is_empty():
    # Bash `case` had no default arm -> echoed nothing.
    assert rlx.github_slug_for("nope") == ""


# --------------------------------------------------------------------------
# compute_new_version
# --------------------------------------------------------------------------


def test_compute_patch():
    assert rlx.compute_new_version("1.2.3", "patch") == "1.2.4"


def test_compute_minor():
    assert rlx.compute_new_version("1.2.3", "minor") == "1.3.0"


def test_compute_major():
    assert rlx.compute_new_version("1.2.3", "major") == "2.0.0"


def test_compute_strips_leading_v_from_current():
    assert rlx.compute_new_version("v1.2.3", "patch") == "1.2.4"


def test_compute_literal_returned_as_is():
    assert rlx.compute_new_version("1.2.3", "9.9.9") == "9.9.9"


def test_compute_literal_leading_v_yields_empty():
    # Faithful bash quirk: the literal-version regex is `^[0-9]+\.…` (no leading
    # 'v'), so 'v9.9.9' does NOT match it and falls through the bump `case`,
    # which has no matching arm -> echoes nothing. (`*.*.*` still passes the
    # earlier *validation* arm, so this combination is reachable.)
    assert rlx.compute_new_version("1.2.3", "v9.9.9") == ""


def test_compute_literal_keeps_prerelease_suffix():
    # The bash regex is a PREFIX match `^[0-9]+\.[0-9]+\.[0-9]+`; a trailing
    # pre-release/build is kept verbatim (the literal is echoed as-is).
    assert rlx.compute_new_version("1.2.3", "2.0.0-rc.1") == "2.0.0-rc.1"


def test_compute_unknown_bump_is_empty():
    # No default arm in the bash `case` for the bump computation.
    assert rlx.compute_new_version("1.2.3", "bogus") == ""


# --------------------------------------------------------------------------
# _looks_like_version (the loose `*.*.*` validation arm)
# --------------------------------------------------------------------------


def test_looks_like_version_true():
    assert rlx._looks_like_version("1.2.3")
    assert rlx._looks_like_version("a.b.c")  # bash `*.*.*` glob is non-numeric
    assert rlx._looks_like_version("1.2.3-rc.1")


def test_looks_like_version_false():
    assert not rlx._looks_like_version("patchy")
    assert not rlx._looks_like_version("1.2")


# --------------------------------------------------------------------------
# parse_only / _is_allowed
# --------------------------------------------------------------------------


def test_parse_only_splits():
    assert rlx.parse_only("comms,lex") == ["comms", "lex"]


def test_parse_only_empty():
    assert rlx.parse_only("") == []


def test_is_allowed_no_filter():
    assert rlx._is_allowed("comms", [], "")


def test_is_allowed_in_list():
    assert rlx._is_allowed("lex", ["comms", "lex"], "comms,lex")


def test_is_allowed_not_in_list():
    assert not rlx._is_allowed("vscode", ["comms", "lex"], "comms,lex")


# --------------------------------------------------------------------------
# _extract_pr_number
# --------------------------------------------------------------------------


def test_extract_pr_number_from_url():
    out = "https://github.com/lex-fmt/comms/pull/42\n"
    assert rlx._extract_pr_number(out) == "42"


def test_extract_pr_number_none():
    assert rlx._extract_pr_number("no pr here") == ""


# --------------------------------------------------------------------------
# _first_database_id  ( `.[0].databaseId // empty` )
# --------------------------------------------------------------------------


def test_first_database_id_present():
    assert rlx._first_database_id('[{"databaseId": 12345}]') == "12345"


def test_first_database_id_empty_array():
    assert rlx._first_database_id("[]") == ""


def test_first_database_id_empty_string():
    assert rlx._first_database_id("") == ""


def test_first_database_id_null():
    assert rlx._first_database_id('[{"databaseId": null}]') == ""


def test_first_database_id_unparseable():
    assert rlx._first_database_id("not json") == ""


# --------------------------------------------------------------------------
# render_status_line
# --------------------------------------------------------------------------


def test_status_line_release_would_happen():
    line = rlx.render_status_line("comms", "1.2.3", 0, "would release: 3 commits")
    assert line == "comms              v1.2.3    ⚠ would release: 3 commits"


def test_status_line_no_release():
    line = rlx.render_status_line("lex", "0.9.0", 1, "up to date")
    assert line == "lex                v0.9.0    ✓ up to date"


def test_status_line_error():
    line = rlx.render_status_line("vscode", "?", 2, "boom")
    assert line == "vscode             v?        ✗ should-release exited 2: boom"


# --------------------------------------------------------------------------
# _parse_args  (data-layer parse, no side effects)
# --------------------------------------------------------------------------


def test_parse_no_args_exits_64(capsys):
    rc = rlx.main([])
    assert rc == 64
    # Bash printed usage to STDOUT on the no-args path.
    assert "Usage:" in capsys.readouterr().out


def test_parse_help_exits_0(capsys):
    rc = rlx.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out


def test_parse_unknown_arg_exits_64(capsys):
    rc = rlx.main(["patch", "--bogus"])
    assert rc == 64
    assert "unknown arg: --bogus" in capsys.readouterr().err


def test_parse_bad_bump_kind_exits_64(capsys, tmp_path):
    # A valid repo is supplied so we get past the no-repos guard, but the
    # bump-kind 'frobnicate' is neither patch/minor/major nor `*.*.*`.
    _make_repo(tmp_path, with_primitives=True)
    rc = rlx.main(["frobnicate", "--comms", str(tmp_path)])
    assert rc == 64
    assert "bad bump-kind" in capsys.readouterr().err


def test_parse_no_repos_exits_64(capsys):
    rc = rlx.main(["patch"])
    assert rc == 64
    assert "no repo paths supplied" in capsys.readouterr().err


def test_status_mode_skips_bump_validation_but_needs_repos(capsys):
    # --status with no repos still hits the no-repos guard (64).
    rc = rlx.main(["--status"])
    assert rc == 64
    assert "no repo paths supplied" in capsys.readouterr().err


# --------------------------------------------------------------------------
# _validate  (path + primitive existence -> exit 1)
# --------------------------------------------------------------------------


def _make_repo(path, *, with_primitives: bool, missing=()):
    """Materialize a repo dir with executable scripts/release/<prim> primitives,
    optionally omitting some to exercise the missing-primitive abort."""
    rel = os.path.join(str(path), "scripts", "release")
    os.makedirs(rel, exist_ok=True)
    if with_primitives:
        prims = [
            "get-current-version",
            "get-commits-since-release",
            "update-release",
            "trigger-release",
        ]
        for prim in prims:
            if prim in missing:
                continue
            p = os.path.join(rel, prim)
            with open(p, "w") as fh:
                fh.write("#!/usr/bin/env bash\n")
            os.chmod(p, os.stat(p).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def test_validate_not_a_directory_exits_1(capsys, tmp_path):
    missing = tmp_path / "nope"
    rc = rlx.main(["patch", "--comms", str(missing)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "not a directory" in err
    assert "(for --comms)" in err


def test_validate_missing_primitive_exits_1(capsys, tmp_path):
    _make_repo(tmp_path, with_primitives=True, missing=("trigger-release",))
    rc = rlx.main(["patch", "--comms", str(tmp_path)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "missing scripts/release/trigger-release" in err
    assert "Layer 0 must be merged in lex-fmt/lex-fmt/comms first" in err


def test_validate_non_executable_primitive_exits_1(capsys, tmp_path):
    _make_repo(tmp_path, with_primitives=True)
    # Strip exec bit from one primitive: the bash `-x` test must fail.
    p = os.path.join(str(tmp_path), "scripts", "release", "update-release")
    os.chmod(p, stat.S_IRUSR | stat.S_IWUSR)
    rc = rlx.main(["patch", "--comms", str(tmp_path)])
    assert rc == 1
    assert "missing scripts/release/update-release" in capsys.readouterr().err
