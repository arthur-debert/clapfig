"""release_lex verb — pure decision logic + arg parsing + sequencing helpers.

Post `scripts/release` retirement: release-lex computes the should-release
decision GENERICALLY via plain git (commits since the last final release tag) and
dispatches each repo via the MAINTAINER's `release-core cut` (resolved from PATH,
run in the repo's cwd — it reads the version from cwd's manifest, computes the
bump, and dispatches cwd's release.yml; CI does the bump/CHANGELOG/commit/tag/
build).
The per-repo `bin/release` and `bin/diff-since-release` dependencies were dropped
— they were absent / stale on chain repos whose mains lag. The live multi-repo
orchestration (fetch/checkout/pull/submodule, release-cut dispatch, gh run
list/watch) is genuine side-effecting glue requiring real repos + GitHub, so it
is NOT unit-tested (that is the script's whole point). NO real release is ever
cut here. Tested: the github-slug map, the generic git should-release decision
(last-final-tag selection + log, mocked at the proc layer — NO real git/network),
the bump-kind / version recognizers, the --only filter, the run-id extractor, the
status-line renderer, and the arg-parse + validation exit codes (release-cut
on PATH stubbed via shutil.which; data layer mocked via tmp dirs)."""

from __future__ import annotations

import os
import shutil

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
    assert rlx.github_slug_for("nope") == ""


def test_repo_name_strips_owner():
    assert rlx._repo_name("tree-sitter") == "tree-sitter-lex"
    assert rlx._repo_name("comms") == "comms"


# --------------------------------------------------------------------------
# _Res / proc mocking — every decision test drives the generic git decision by
# stubbing `proc.run` so NO real git/network is ever touched.
# --------------------------------------------------------------------------


class _Res:
    """Stand-in for subprocess.CompletedProcess (the proc.run shape we use)."""

    def __init__(self, returncode: int, stdout: str = "", stderr: str = ""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def _patch_git(monkeypatch, *, tags=None, log=None):
    """Stub the proc layer so `decide_release`'s two git calls return canned
    results. ``tags`` / ``log`` are _Res for the `git tag --list …` and
    `git --no-pager log …` invocations respectively; anything else (e.g. a
    best-effort fetch) gets a benign rc 0. Recognizes the calls by argv shape:
    `git … tag …` and `git … log …`."""

    def fake_run(cmd, **kw):
        if "tag" in cmd:
            return tags if tags is not None else _Res(0, stdout="")
        if "log" in cmd:
            return log if log is not None else _Res(0, stdout="")
        return _Res(0)

    monkeypatch.setattr(rlx.proc, "run", fake_run)


# --------------------------------------------------------------------------
# latest_final_tag  (the last-final-tag selection — drops prereleases)
# --------------------------------------------------------------------------


def test_latest_final_tag_picks_first_non_prerelease():
    # `git tag --sort=-version:refname` already emits highest-first.
    assert rlx.latest_final_tag("v1.2.3\nv1.2.2\nv1.0.0\n") == "v1.2.3"


def test_latest_final_tag_skips_prereleases():
    # A prerelease (contains `-`) above the latest final is skipped.
    assert rlx.latest_final_tag("v2.0.0-rc.1\nv1.9.0\nv1.8.0\n") == "v1.9.0"


def test_latest_final_tag_empty_when_only_prereleases():
    assert rlx.latest_final_tag("v1.0.0-rc.2\nv1.0.0-rc.1\n") == ""


def test_latest_final_tag_empty_when_no_tags():
    assert rlx.latest_final_tag("") == ""


# --------------------------------------------------------------------------
# decide_release  (the generic git should-release decision, mocked at proc)
# --------------------------------------------------------------------------


def test_decide_no_final_tags_is_notags(monkeypatch):
    # No tags at all -> NOTAGS (a first release is human-driven).
    _patch_git(monkeypatch, tags=_Res(0, stdout=""))
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.NOTAGS


def test_decide_only_prerelease_tags_is_notags(monkeypatch):
    # Prerelease-only tags are treated as no final tags -> NOTAGS.
    _patch_git(monkeypatch, tags=_Res(0, stdout="v1.0.0-rc.2\nv1.0.0-rc.1\n"))
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.NOTAGS


def test_decide_final_tag_with_commits_is_release(monkeypatch):
    _patch_git(
        monkeypatch,
        tags=_Res(0, stdout="v1.2.3\nv1.2.2\n"),
        log=_Res(0, stdout="abc1234 feat: a thing\ndef5678 fix: another\n"),
    )
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.RELEASE
    assert d.count == 2
    assert d.tag == "v1.2.3"


def test_decide_final_tag_no_commits_is_uptodate(monkeypatch):
    _patch_git(monkeypatch, tags=_Res(0, stdout="v1.2.3\n"), log=_Res(0, stdout="\n\n"))
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.UPTODATE
    assert d.tag == "v1.2.3"


def test_decide_tag_listing_error_surfaces_loudly(monkeypatch):
    # A genuine git failure on the tag listing -> ERROR, NOT masked as NOTAGS.
    _patch_git(monkeypatch, tags=_Res(128, stderr="fatal: not a git repository"))
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.ERROR
    assert d.rc == 128
    assert "not a git repository" in d.stderr


def test_decide_log_error_surfaces_loudly(monkeypatch):
    # tag listing OK but the log fails -> ERROR (never read as "nothing").
    _patch_git(
        monkeypatch,
        tags=_Res(0, stdout="v1.2.3\n"),
        log=_Res(128, stderr="fatal: bad revision"),
    )
    d = rlx.decide_release("/tmp/x")
    assert d.state == rlx.ERROR
    assert d.rc == 128
    assert "bad revision" in d.stderr


# --------------------------------------------------------------------------
# _looks_like_version (the loose `*.*.*` validation arm)
# --------------------------------------------------------------------------


def test_looks_like_version_true():
    assert rlx._looks_like_version("1.2.3")
    assert rlx._looks_like_version("a.b.c")
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
    line = rlx.render_status_line("comms", rlx.Decision(rlx.RELEASE, count=3, tag="v1.2.3"))
    assert line == "comms              ⚠ would release: 3 commit(s) since v1.2.3"


def test_status_line_up_to_date():
    line = rlx.render_status_line("lex", rlx.Decision(rlx.UPTODATE, tag="v1.2.3"))
    assert line == "lex                ✓ up to date (no commits since v1.2.3)"


def test_status_line_no_tags():
    # NOTAGS is the benign "no final release tags yet" case.
    line = rlx.render_status_line("vscode", rlx.Decision(rlx.NOTAGS))
    assert line == "vscode             ✗ no final release tags yet (first release is human-driven)"


def test_status_line_genuine_error_is_not_no_tags():
    # ERROR is a real failure (e.g. git error 128) — it must NOT be rendered as
    # "no tags"; that would mask a real problem from an operator.
    line = rlx.render_status_line("nvim", rlx.Decision(rlx.ERROR, rc=128))
    assert "no final release tags yet" not in line
    assert line == "nvim               ✗ should-release decision FAILED (git exited 128)"


# --------------------------------------------------------------------------
# _parse_args  (data-layer parse, no side effects)
# --------------------------------------------------------------------------


def test_parse_no_args_exits_64(capsys):
    rc = rlx.main([])
    assert rc == 64
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
    # bad bump-kind is rejected before any PATH / path validation.
    _make_repo(tmp_path)
    rc = rlx.main(["frobnicate", "--comms", str(tmp_path)])
    assert rc == 64
    assert "bad bump-kind" in capsys.readouterr().err


def test_parse_no_repos_exits_64(capsys):
    rc = rlx.main(["patch"])
    assert rc == 64
    assert "no repo paths supplied" in capsys.readouterr().err


def test_status_mode_skips_bump_validation_but_needs_repos(capsys):
    rc = rlx.main(["--status"])
    assert rc == 64
    assert "no repo paths supplied" in capsys.readouterr().err


# --------------------------------------------------------------------------
# Relative-path resolution (the order-independent-walk invariant)
#
# release-lex iterates the 6 repos and os.chdir's into each. If a repo path is
# relative, it MUST be resolved to an absolute path up front at parse time —
# otherwise the second repo's relative path would resolve against the FIRST
# repo's dir after the first chdir, breaking the cascade for relative input.
# --------------------------------------------------------------------------


def test_parse_args_resolves_relative_repo_paths_to_absolute(tmp_path, monkeypatch):
    # Lay out the 6 repos as siblings under tmp_path, then cd into tmp_path and
    # pass each as a RELATIVE path (`./comms`, `lex`, …). After parsing, every
    # repo path must be absolute and point at the right dir.
    rels = {
        "comms": "./comms",
        "lex": "lex",
        "tree-sitter": "./nested/tree-sitter",
        "vscode": "vscode",
        "nvim": "./nvim",
        "lexed": "lexed",
    }
    for rel in rels.values():
        (tmp_path / rel).mkdir(parents=True, exist_ok=True)
    monkeypatch.chdir(tmp_path)

    argv = ["patch"]
    for key, rel in rels.items():
        argv += [f"--{key}", rel]
    cfg = rlx._parse_args(argv)

    for key, rel in rels.items():
        got = cfg["repos"][key]
        assert os.path.isabs(got), f"{key} path not absolute: {got!r}"
        assert os.path.realpath(got) == os.path.realpath(tmp_path / rel)


def test_relative_paths_resolve_independent_of_iteration_order(tmp_path, monkeypatch):
    # The core regression: with relative input, chdir'ing into repo[0] must NOT
    # change how repo[1..n] resolve. We parse relative paths once (resolving them
    # to absolute), then simulate the walk — chdir into each repo in turn — and
    # assert decide_release is always invoked with the repo's ORIGINAL absolute
    # path, never one mangled by a previous chdir (e.g. `lex/vscode`).
    rels = {"comms": "./comms", "lex": "lex", "vscode": "./vscode"}
    for rel in rels.values():
        (tmp_path / rel).mkdir(parents=True, exist_ok=True)
        # Make each a "git repo" enough for the real os.chdir to succeed.
    monkeypatch.chdir(tmp_path)

    argv = ["patch"]
    for key, rel in rels.items():
        argv += [f"--{key}", rel]
    cfg = rlx._parse_args(argv)

    expected_abs = {key: os.path.realpath(tmp_path / rel) for key, rel in rels.items()}

    # Real os.chdir (the trap is order-dependent cwd); record the path
    # decide_release receives for each repo and confirm cwd actually moved.
    seen_paths: dict[str, str] = {}
    seen_cwd: dict[str, str] = {}

    def fake_decide(path):
        # Capture which key this is by matching the absolute path.
        for k, p in expected_abs.items():
            if os.path.realpath(path) == p:
                seen_paths[k] = path
                seen_cwd[k] = os.path.realpath(os.getcwd())
        return rlx.Decision(rlx.NOTAGS)

    monkeypatch.setattr(rlx, "decide_release", fake_decide)

    # Walk the repos in ORDER, chdir'ing into each (the real cascade behavior).
    for key in ("comms", "lex", "vscode"):
        path = cfg["repos"][key]
        assert os.path.isabs(path)
        os.chdir(path)  # this would mangle a relative repo[next] if unresolved
        rlx.decide_release(path)

    # Every repo was visited with its own absolute path, regardless of the
    # cwd left behind by the previous repo's chdir.
    for key in ("comms", "lex", "vscode"):
        assert seen_paths[key] == cfg["repos"][key]
        assert os.path.realpath(seen_paths[key]) == expected_abs[key]
        assert seen_cwd[key] == expected_abs[key]


# --------------------------------------------------------------------------
# _validate  (path + managed-tool existence -> exit 1)
# --------------------------------------------------------------------------


def _make_repo(path):
    """Materialize a bare repo dir. release-lex no longer requires any per-repo
    bin/ tool — cut mode dispatches via the maintainer's `release-cut` on PATH
    and the should-release decision is generic git — so the repo is just a
    directory that must exist."""
    os.makedirs(str(path), exist_ok=True)


@pytest.fixture
def _release_core_on_path(monkeypatch):
    """Pretend `release-core` is on PATH so cut-mode validation gets past the
    up-front PATH check and reaches the per-repo path validation under test."""
    real_which = shutil.which

    def fake_which(name, *a, **k):
        if name == rlx.RELEASE_CORE:
            return "/fake/bin/release-core"
        return real_which(name, *a, **k)

    monkeypatch.setattr(rlx.shutil, "which", fake_which)


def test_validate_release_core_not_on_path_exits_1(capsys, tmp_path, monkeypatch):
    # Cut mode requires `release-core` on the maintainer's PATH, checked once up
    # front. Absent it -> exit 1 with a clear maintainer-facing message.
    _make_repo(tmp_path)
    monkeypatch.setattr(rlx.shutil, "which", lambda *a, **k: None)
    rc = rlx.main(["patch", "--comms", str(tmp_path)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "release-core not on PATH" in err
    assert "add the release repo's bin/ to PATH" in err


def test_validate_not_a_directory_exits_1(capsys, tmp_path, _release_core_on_path):
    missing = tmp_path / "nope"
    rc = rlx.main(["patch", "--comms", str(missing)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "not a directory" in err
    assert "(for --comms)" in err


def test_validate_cut_mode_needs_no_per_repo_bin_tool(tmp_path, _release_core_on_path):
    # With release-core on PATH, a bare repo dir (no bin/ at all) passes cut-mode
    # validation — no per-repo bin/release is required anymore.
    _make_repo(tmp_path)
    cfg = {
        "status_mode": False,
        "bump_kind": "patch",
        "dry_run": False,
        "only": "",
        "repos": {"comms": str(tmp_path)},
    }
    assert rlx._validate(cfg) is None


def test_validate_stores_resolved_absolute_release_core_path(tmp_path, monkeypatch):
    # _validate must resolve release-core to an ABSOLUTE path ONCE (before any
    # os.chdir) and stash it in cfg["release_core_path"]; dispatch then uses that
    # rather than re-resolving the bare name against a changed cwd. Same spirit
    # as the #404 abs-path fix.
    _make_repo(tmp_path)
    monkeypatch.setattr(rlx.shutil, "which", lambda name, *a, **k: "/abs/bin/release-core")
    cfg = {
        "status_mode": False,
        "bump_kind": "patch",
        "dry_run": False,
        "only": "",
        "repos": {"comms": str(tmp_path)},
    }
    assert rlx._validate(cfg) is None
    assert cfg["release_core_path"] == "/abs/bin/release-core"
    assert os.path.isabs(cfg["release_core_path"])


def test_validate_status_mode_does_not_store_release_core_path(tmp_path):
    # Read-only --status mode never dispatches, so it must NOT consult PATH or
    # set release_core_path.
    _make_repo(tmp_path)
    cfg = {
        "status_mode": True,
        "bump_kind": "status",
        "dry_run": False,
        "only": "",
        "repos": {"comms": str(tmp_path)},
    }
    assert rlx._validate(cfg) is None
    assert "release_core_path" not in cfg


def test_validate_status_mode_needs_no_bin_tool(tmp_path):
    # In --status mode the decision is generic git and there is no dispatch — a
    # repo with no bin/ tool (just a directory) passes validation, and PATH is
    # not consulted (no release-cut required for read-only status).
    _make_repo(tmp_path)
    cfg = {
        "status_mode": True,
        "bump_kind": "status",
        "dry_run": False,
        "only": "",
        "repos": {"comms": str(tmp_path)},
    }
    assert rlx._validate(cfg) is None


# --------------------------------------------------------------------------
# _release_one — generic git decision routing (the should-release gate)
#
# We mock the proc layer so no real git/network/release ever runs. `_run`
# (git fetch/checkout/pull) and os.chdir are stubbed; only the canned git tag
# listing + log (via _patch_git) drive the branch under test.
# --------------------------------------------------------------------------


def _patch_release_one_env(monkeypatch, *, tags=None, log=None):
    """Neutralize all side effects of _release_one except the git decision.

    `proc.run` routes git tag/log to canned _Res via _patch_git; `_run` and
    os.chdir are no-ops; os.path.isfile(.gitmodules) is False; ./bin/release is
    never reached in the no-release branches (the decision short-circuits)."""
    _patch_git(monkeypatch, tags=tags, log=log)
    monkeypatch.setattr(rlx, "_run", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os.path, "isfile", lambda *a, **k: False)


_CFG = {"dry_run": False, "bump_kind": "patch", "repos": {"comms": "/tmp/x"}}


def test_release_one_no_tags_skips_cleanly(monkeypatch, capsys):
    # (a) no final tags -> benign skip, returns 0, says "no final release tags".
    _patch_release_one_env(monkeypatch, tags=_Res(0, stdout=""))
    rc = rlx._release_one("comms", _CFG)
    assert rc == 0
    out = capsys.readouterr().out
    assert "no final release tags yet" in out


def test_release_one_prerelease_only_tags_skips_cleanly(monkeypatch, capsys):
    # Prerelease-only tags are treated as no-final-tags -> benign skip.
    _patch_release_one_env(monkeypatch, tags=_Res(0, stdout="v1.0.0-rc.1\n"))
    rc = rlx._release_one("comms", _CFG)
    assert rc == 0
    assert "no final release tags yet" in capsys.readouterr().out


def test_release_one_genuine_error_is_surfaced_not_masked(monkeypatch, capsys):
    # (b) git error (e.g. 128) -> propagate the failure, NOT a 0 skip.
    _patch_release_one_env(monkeypatch, tags=_Res(128, stderr="fatal: not a git repository"))
    rc = rlx._release_one("comms", _CFG)
    assert rc == 128  # the real error code, surfaced — not 0
    err = capsys.readouterr().err
    assert "FAILED" in err
    assert "fatal: not a git repository" in err


def test_release_one_success_empty_log_is_nothing_to_release(monkeypatch, capsys):
    # (c) final tag + empty log -> nothing to release, clean skip.
    _patch_release_one_env(monkeypatch, tags=_Res(0, stdout="v1.2.3\n"), log=_Res(0, stdout=""))
    rc = rlx._release_one("comms", _CFG)
    assert rc == 0
    assert "no new commits since v1.2.3" in capsys.readouterr().out


def test_release_one_success_with_commits_proceeds_to_dispatch(monkeypatch, capsys):
    # (d) final tag + commits -> decision passes; in dry-run we stop at the
    # dispatch echo (returns 0) without cutting a real release. The dispatched
    # version is TAG-derived (v1.2.3 + patch -> 1.2.4), not the bump-kind.
    cfg = {**_CFG, "dry_run": True}
    _patch_release_one_env(
        monkeypatch,
        tags=_Res(0, stdout="v1.2.3\n"),
        log=_Res(0, stdout="abc1234 feat: a\ndef5678 fix: b\n"),
    )
    rc = rlx._release_one("comms", cfg)
    assert rc == 0
    out = capsys.readouterr().out
    assert "2 commit(s) since v1.2.3" in out
    # The explicit derived version is dispatched — NOT the bump-kind.
    assert "release-core cut 1.2.4" in out
    assert "release-core cut patch" not in out


def test_release_one_dispatch_uses_resolved_path_via_proc_run(monkeypatch, capsys):
    # The live (non-dry-run) dispatch must invoke the ABSOLUTE release-core path
    # stashed by _validate, with the `cut` subcommand, routed through the
    # centralized proc.run chokepoint — not the bare name via subprocess.run
    # directly. We make release-core cut return nonzero so _release_one returns
    # right after dispatch (no gh/sleep needed).
    cfg = {
        **_CFG,
        "dry_run": False,
        "release_core_path": "/abs/bin/release-core",
    }
    captured: dict[str, list[str]] = {}

    def fake_run(cmd, **kw):
        if "tag" in cmd:
            return _Res(0, stdout="v1.2.3\n")
        if "log" in cmd:
            return _Res(0, stdout="abc1234 feat: a\n")
        # The release-core cut dispatch: capture argv + kwargs, fail it to bail.
        captured["cmd"] = cmd
        captured["kw"] = kw
        return _Res(1)

    monkeypatch.setattr(rlx.proc, "run", fake_run)
    monkeypatch.setattr(rlx, "_run", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os.path, "isfile", lambda *a, **k: False)

    rc = rlx._release_one("comms", cfg)
    assert rc == 1  # release-core cut failed -> surfaced, not a silent 0
    # TAG-authoritative: dispatches the EXPLICIT version (v1.2.3 + patch = 1.2.4),
    # NOT the bump-kind, so a drifted manifest can't drive the version.
    assert captured["cmd"] == ["/abs/bin/release-core", "cut", "1.2.4"]
    # Honors the proc.run contract used for a live, streaming dispatch.
    assert captured["kw"].get("check") is False
    assert captured["kw"].get("capture_output") is False
    assert "release-core cut 1.2.4 failed" in capsys.readouterr().err


def test_release_one_malformed_tag_fails_cleanly_no_traceback(monkeypatch, capsys):
    # A final tag that isn't strict 3-part semver (e.g. `v1.2`) makes
    # next_version -> version.parse raise ValueError. _release_one must catch
    # it, print a clean `✗ failed to parse tag ...` to stderr, and return 1 —
    # NEVER let the traceback crash the orchestrator, and NEVER dispatch.
    cfg = {
        **_CFG,
        "dry_run": False,
        "release_core_path": "/abs/bin/release-core",
    }
    dispatched: list[list[str]] = []

    def fake_run(cmd, **kw):
        if "tag" in cmd:
            return _Res(0, stdout="v1.2\n")  # malformed: not X.Y.Z
        if "log" in cmd:
            return _Res(0, stdout="abc1234 feat: a\n")
        dispatched.append(cmd)  # release-core cut must NOT be reached
        return _Res(0)

    monkeypatch.setattr(rlx.proc, "run", fake_run)
    monkeypatch.setattr(rlx, "_run", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os.path, "isfile", lambda *a, **k: False)

    rc = rlx._release_one("comms", cfg)
    assert rc == 1  # clean failure, not a traceback crash
    assert dispatched == []  # no release-core cut dispatch on an unparseable tag
    err = capsys.readouterr().err
    assert "failed to parse tag 'v1.2' as semver" in err


# --------------------------------------------------------------------------
# next_version — the tag-authoritative version derivation (the vscode fix)
#
# release-lex DECIDES off the latest final tag, so it must DERIVE the next version
# from that same tag and dispatch it EXPLICITLY — never let release-cut recompute
# from a (possibly drifted) manifest.
# --------------------------------------------------------------------------


def test_next_version_patch_from_tag():
    assert rlx.next_version("patch", "v0.10.8") == "0.10.9"


def test_next_version_minor_from_tag():
    # minor zeroes patch (semver).
    assert rlx.next_version("minor", "v0.10.8") == "0.11.0"


def test_next_version_major_from_tag():
    # major zeroes minor+patch (semver).
    assert rlx.next_version("major", "v0.10.8") == "1.0.0"


def test_next_version_strips_prerelease_on_tag():
    # parse() tolerates a prerelease suffix on the tag; bump strips it.
    assert rlx.next_version("patch", "v1.0.0-rc.1") == "1.0.1"


def test_next_version_explicit_xyz_passes_through():
    # An explicit X.Y.Z bump-kind is dispatched verbatim — no tag math.
    assert rlx.next_version("1.2.3", "v0.10.8") == "1.2.3"


# --------------------------------------------------------------------------
# The vscode regression (the key test): a STALE manifest must NOT drive the
# version. release-lex derives from the TAG and dispatches it explicitly.
# --------------------------------------------------------------------------


def test_vscode_stale_manifest_does_not_drive_version(monkeypatch):
    # vscode's package.json froze at 0.4.1-rc.1 ~25 releases ago while its real
    # version is the tag v0.10.8. We mock that stale manifest reader to PROVE it
    # is never consulted: the dispatched version is TAG-derived (0.10.9), not the
    # manifest-derived 0.4.2.
    # Spy on version.parse to PROVE it is fed the TAG, never the stale
    # manifest version '0.4.1-rc.1' (which would yield 0.4.2).
    parse_inputs: list[str] = []
    monkeypatch.setattr(rlx.version, "parse", _spy_parse(parse_inputs, rlx.version.parse))

    cfg = {
        "dry_run": False,
        "bump_kind": "patch",
        "release_core_path": "/abs/bin/release-core",
        "repos": {"vscode": "/tmp/vscode"},
    }
    captured: dict[str, list[str]] = {}

    def fake_run(cmd, **kw):
        if "tag" in cmd:
            # vscode's latest FINAL tag (stray rc/pre tags deleted upstream).
            return _Res(0, stdout="v0.10.8\n")
        if "log" in cmd:
            return _Res(0, stdout="abc1234 feat: a\n")
        captured["cmd"] = cmd
        return _Res(1)  # fail the dispatch to bail right after it

    monkeypatch.setattr(rlx.proc, "run", fake_run)
    monkeypatch.setattr(rlx, "_run", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os.path, "isfile", lambda *a, **k: False)

    rlx._release_one("vscode", cfg)
    # The dispatched version is derived from the TAG (0.10.9), NOT the stale
    # manifest's 0.4.2 — this is the regression the fix prevents.
    assert captured["cmd"] == ["/abs/bin/release-core", "cut", "0.10.9"]
    assert captured["cmd"][2] != "0.4.2"
    # version.parse was fed the TAG, never the stale manifest's 0.4.1-rc.1.
    assert parse_inputs == ["v0.10.8"]
    assert "0.4.1-rc.1" not in parse_inputs


def _spy_parse(log, real):
    """Wrap version.parse so we can confirm it is fed the TAG (not a manifest
    version). Records each input string into ``log``."""

    def wrapper(s):
        log.append(s)
        return real(s)

    return wrapper


def test_vscode_parse_is_fed_the_tag_not_the_manifest(monkeypatch):
    # Stronger: confirm version.parse is called with the TAG string 'v0.10.8',
    # never the stale manifest version '0.4.1-rc.1'.
    seen: list[str] = []
    monkeypatch.setattr(rlx.version, "parse", _spy_parse(seen, rlx.version.parse))
    out = rlx.next_version("patch", "v0.10.8")
    assert out == "0.10.9"
    assert seen == ["v0.10.8"]
    assert "0.4.1-rc.1" not in seen


def test_notags_decision_never_guesses_a_version(monkeypatch, capsys):
    # NOTAGS short-circuits BEFORE next_version — release-lex never guesses a
    # first version. A repo with no final tag is skipped cleanly (no dispatch).
    cfg = {
        "dry_run": False,
        "bump_kind": "patch",
        "release_core_path": "/abs/bin/release-core",
        "repos": {"nvim": "/tmp/nvim"},
    }
    dispatched: list[str] = []

    def fake_run(cmd, **kw):
        if "tag" in cmd:
            return _Res(0, stdout="")  # no tags at all
        dispatched.append(cmd)
        return _Res(0)

    monkeypatch.setattr(rlx.proc, "run", fake_run)
    monkeypatch.setattr(rlx, "_run", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)
    monkeypatch.setattr(rlx.os.path, "isfile", lambda *a, **k: False)

    rc = rlx._release_one("nvim", cfg)
    assert rc == 0
    # No release-core cut dispatch happened (only the tag listing ran).
    assert not any("release-core" in str(c) for c in dispatched)
    assert "no final release tags yet" in capsys.readouterr().out


# --------------------------------------------------------------------------
# _status_one — same routing in read-only --status mode
# --------------------------------------------------------------------------


def _patch_status_one_env(monkeypatch, *, tags=None, log=None):
    _patch_git(monkeypatch, tags=tags, log=log)
    monkeypatch.setattr(rlx.os, "chdir", lambda *a, **k: None)


def test_status_one_no_tags(monkeypatch, capsys):
    _patch_status_one_env(monkeypatch, tags=_Res(0, stdout=""))
    rlx._status_one("comms", {"repos": {"comms": "/tmp/x"}})
    out = capsys.readouterr().out
    assert "no final release tags yet" in out


def test_status_one_genuine_error_surfaces_stderr(monkeypatch, capsys):
    _patch_status_one_env(monkeypatch, tags=_Res(128, stderr="fatal: bad object HEAD"))
    rlx._status_one("comms", {"repos": {"comms": "/tmp/x"}})
    captured = capsys.readouterr()
    assert "FAILED (git exited 128)" in captured.out
    assert "no final release tags yet" not in captured.out
    assert "fatal: bad object HEAD" in captured.err
