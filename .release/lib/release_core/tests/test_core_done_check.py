"""done_check verb — stack detection, status aggregation, emitters. Pure layer.

The contract oracles (workflow-name→stack map, aggregate(), render_json/table)
are tested directly. main() is exercised by monkeypatching the gh boundary +
the check_* helpers — mocking at the data layer, never at subprocess (the
shell-to-python contract's rule). No network, no gh.
"""

from __future__ import annotations

import base64
import json

from release_core import gh
from release_core.verbs import done_check


def _b64(text: str) -> dict:
    return {"content": base64.b64encode(text.encode()).decode(), "encoding": "base64"}


# --- workflow-name → stack mapping -----------------------------------


def test_workflow_to_stack_canonical_uses_line():
    rel = "    uses: arthur-debert/release/.github/workflows/rust-cli.yml@v1\n"
    assert done_check._workflow_name_from_release_yml(rel) == "rust-cli.yml"


def test_workflow_to_stack_self_call_line():
    rel = "    uses: ./.github/workflows/gh-action.yml\n"
    assert done_check._workflow_name_from_release_yml(rel) == "gh-action.yml"


def test_workflow_to_stack_no_match():
    assert done_check._workflow_name_from_release_yml("name: ci\non: push\n") is None


def test_canonical_preferred_over_self_call():
    rel = (
        "    uses: ./.github/workflows/gh-action.yml\n"
        "    uses: arthur-debert/release/.github/workflows/rust-cli.yml@v1\n"
    )
    # canonical matched first regardless of file order
    assert done_check._workflow_name_from_release_yml(rel) == "rust-cli.yml"


def test_every_stack_has_a_release_workflow_and_verbs():
    for stack in done_check._RELEASE_WORKFLOW_FOR_STACK:
        assert stack in done_check._VERBS_FOR_STACK
        assert "release" in done_check._VERBS_FOR_STACK[stack]


# --- detect_stack: primary (release.yml) + fallback (filesystem) -----


def test_detect_stack_via_release_yml(monkeypatch):
    monkeypatch.setattr(
        done_check,
        "_raw_contents",
        lambda r, p, ref=None: "uses: arthur-debert/release/.github/workflows/go-cli.yml@v1",
    )
    assert done_check.detect_stack("o/r") == "go-cli"


def test_detect_stack_fallback_rust_lib(monkeypatch):
    monkeypatch.setattr(done_check, "_raw_contents", lambda r, p, ref=None: None)
    monkeypatch.setattr(done_check, "_root_names", lambda r: ["Cargo.toml", "src"])
    monkeypatch.setattr(done_check, "_file_exists", lambda r, p, ref=None: False)
    assert done_check.detect_stack("o/r") == "rust-lib"


def test_detect_stack_fallback_brew_tap(monkeypatch):
    monkeypatch.setattr(done_check, "_raw_contents", lambda r, p, ref=None: None)
    monkeypatch.setattr(done_check, "_root_names", lambda r: ["Formula", "README.md"])
    assert done_check.detect_stack("o/r") == "brew-tap"


def test_detect_stack_fallback_electron(monkeypatch):
    monkeypatch.setattr(done_check, "_root_names", lambda r: ["package.json"])
    monkeypatch.setattr(done_check, "_file_exists", lambda r, p, ref=None: False)

    def raw(r, p, ref=None):
        if p == ".github/workflows/release.yml":
            return None
        if p == "package.json":
            return '{"devDependencies":{"electron":"^30"}}'
        return None

    monkeypatch.setattr(done_check, "_raw_contents", raw)
    assert done_check.detect_stack("o/r") == "electron-app"


def test_detect_stack_unreadable_contents_raises(monkeypatch):
    monkeypatch.setattr(done_check, "_raw_contents", lambda r, p, ref=None: None)
    monkeypatch.setattr(done_check, "_root_names", lambda r: None)
    import pytest

    with pytest.raises(done_check.StackError):
        done_check.detect_stack("o/r")


# --- check_ci aggregation over recorded runs JSON --------------------


def test_check_ci_picks_most_recent_completed(monkeypatch):
    monkeypatch.setattr(done_check, "_default_branch", lambda r: "main")

    def rest(path, **kw):
        return {
            "total_count": 2,
            "workflow_runs": [
                {"status": "in_progress", "conclusion": None},
                {"status": "completed", "conclusion": "success"},
            ],
        }

    monkeypatch.setattr(gh, "rest", rest)
    assert done_check.check_ci("o/r") == "PASS|ci.yml"


def test_check_ci_failure(monkeypatch):
    monkeypatch.setattr(done_check, "_default_branch", lambda r: "main")
    monkeypatch.setattr(
        gh,
        "rest",
        lambda p, **k: {
            "total_count": 1,
            "workflow_runs": [{"status": "completed", "conclusion": "failure"}],
        },
    )
    assert done_check.check_ci("o/r") == "FAIL|ci.yml last completed run = failure"


def test_check_ci_all_in_progress_warns(monkeypatch):
    monkeypatch.setattr(done_check, "_default_branch", lambda r: "main")
    monkeypatch.setattr(
        gh,
        "rest",
        lambda p, **k: {
            "total_count": 1,
            "workflow_runs": [{"status": "queued", "conclusion": None}],
        },
    )
    out = done_check.check_ci("o/r")
    assert out == "WARN|ci.yml has runs but none completed (all queued/in-progress)"


def test_check_ci_falls_through_to_test_yml(monkeypatch):
    monkeypatch.setattr(done_check, "_default_branch", lambda r: "trunk")

    def rest(path, **kw):
        if "ci.yml" in path:
            return {"total_count": 0, "workflow_runs": []}
        if "test.yml" in path:
            return {
                "total_count": 1,
                "workflow_runs": [{"status": "completed", "conclusion": "success"}],
            }
        return {}

    monkeypatch.setattr(gh, "rest", rest)
    assert done_check.check_ci("o/r") == "PASS|test.yml"


def test_check_ci_no_runs_warns(monkeypatch):
    monkeypatch.setattr(done_check, "_default_branch", lambda r: "main")
    monkeypatch.setattr(gh, "rest", lambda p, **k: {"total_count": 0, "workflow_runs": []})
    assert done_check.check_ci("o/r") == "WARN|no ci.yml or test.yml runs found on main"


# --- check_release ---------------------------------------------------


def test_check_release_pass_canonical(monkeypatch):
    monkeypatch.setattr(gh, "rest", lambda p, **k: {"tag_name": "v1.2.3"})
    monkeypatch.setattr(
        done_check,
        "_raw_contents",
        lambda r, p, ref=None: "uses: arthur-debert/release/.github/workflows/rust-cli.yml@v1",
    )
    assert done_check.check_release("o/r", "rust-cli") == "PASS|v1.2.3 via rust-cli.yml"


def test_check_release_self_call(monkeypatch):
    monkeypatch.setattr(gh, "rest", lambda p, **k: {"tag_name": "v2.0.0"})
    monkeypatch.setattr(
        done_check,
        "_raw_contents",
        lambda r, p, ref=None: "    uses: ./.github/workflows/gh-action.yml",
    )
    out = done_check.check_release("o/r", "gh-action")
    assert out == "PASS|v2.0.0 via self-call gh-action.yml"


def test_check_release_bespoke_fails(monkeypatch):
    monkeypatch.setattr(gh, "rest", lambda p, **k: {"tag_name": "v9"})
    monkeypatch.setattr(done_check, "_raw_contents", lambda r, p, ref=None: "run: cargo publish")
    out = done_check.check_release("o/r", "rust-cli")
    assert out == "FAIL|v9 was NOT cut by rust-cli.yml@v1 (release.yml is bespoke)"


def test_check_release_no_releases_warns(monkeypatch):
    def rest(path, **kw):
        if path.endswith("/releases/latest"):
            raise gh.GhError("404")
        if "releases?per_page=1" in path:
            return []
        return {}

    monkeypatch.setattr(gh, "rest", rest)
    out = done_check.check_release("o/r", "rust-cli")
    assert out == "WARN|no releases — consumer has not shipped through canonical yet"


def test_check_release_prerelease_fallback(monkeypatch):
    def rest(path, **kw):
        if path.endswith("/releases/latest"):
            raise gh.GhError("404")
        if "releases?per_page=1" in path:
            return [{"tag_name": "v0.1.0-rc.1"}]
        return {}

    monkeypatch.setattr(gh, "rest", rest)
    monkeypatch.setattr(
        done_check,
        "_raw_contents",
        lambda r, p, ref=None: "uses: arthur-debert/release/.github/workflows/rust-cli.yml@v1",
    )
    assert done_check.check_release("o/r", "rust-cli") == "PASS|v0.1.0-rc.1 via rust-cli.yml"


def test_check_release_missing_yml_at_tag(monkeypatch):
    monkeypatch.setattr(gh, "rest", lambda p, **k: {"tag_name": "v1"})
    monkeypatch.setattr(done_check, "_raw_contents", lambda r, p, ref=None: None)
    out = done_check.check_release("o/r", "rust-cli")
    assert out == "FAIL|no release.yml at tag v1 (release source unknown)"


# --- aggregate: the core verdict + exit-code policy ------------------


def _agg(stack, ci, per_verb):
    return done_check.aggregate("o/r", stack, ci, per_verb)


def test_aggregate_all_pass_is_pilot_running():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "PASS|bin/check",
            "build": "PASS|bin/build",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    assert res["state"] == "pilot-running"
    assert res["exit_code"] == 0
    assert [r["state"] for r in res["rows"]] == ["PASS", "PASS", "PASS"]


def test_aggregate_local_fail_is_implemented_exit1():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "FAIL|bin/check missing",
            "build": "PASS|bin/build",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    assert res["state"] == "implemented"
    assert res["exit_code"] == 1
    assert res["rows"][0]["state"] == "FAIL"
    assert res["rows"][0]["msg"] == "local: bin/check missing; ci: ci.yml"


def test_aggregate_ci_warn_only_is_warnings_exit2():
    res = _agg(
        "nvim-plugin",
        "WARN|no ci.yml or test.yml runs found on main",
        {
            "check": "PASS|bin/check",
            "release": "PASS|v1 via nvim-plugin.yml",
            "release:local": "PASS|bin/release",
        },
    )
    assert res["state"] == "implemented+warnings"
    assert res["exit_code"] == 2


def test_aggregate_release_row_carries_local_and_na_ci():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "PASS|bin/check",
            "build": "PASS|bin/build",
            "release": "WARN|no releases — consumer has not shipped through canonical yet",
            "release:local": "FAIL|bin/release missing",
        },
    )
    rel_row = res["rows"][-1]
    assert rel_row["verb"] == "release"
    assert rel_row["ci"] == "(n/a)"
    assert rel_row["local"] == "FAIL"
    assert "[local: bin/release missing]" in rel_row["msg"]
    assert res["exit_code"] == 2  # WARN release, no FAIL


def test_aggregate_fail_dominates_warn():
    res = _agg(
        "rust-cli",
        "WARN|no ci.yml or test.yml runs found on main",
        {
            "check": "PASS|bin/check",
            "build": "FAIL|bin/build missing",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    assert res["exit_code"] == 1
    assert res["state"] == "implemented"


# --- emitters --------------------------------------------------------


def test_render_json_shape_and_escaping():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "PASS|bin/check",
            "build": "PASS|bin/build",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    out = done_check.render_json(res)
    parsed = json.loads(out)
    assert parsed["repo"] == "o/r"
    assert parsed["stack"] == "rust-cli"
    assert parsed["state"] == "pilot-running"
    assert [v["verb"] for v in parsed["verbs"]] == ["check", "build", "release"]
    assert parsed["verbs"][2]["ci"] == "(n/a)"


def test_render_table_has_header_and_arrow():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "PASS|bin/check",
            "build": "PASS|bin/build",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    out = done_check.render_table(res, quiet=False)
    assert "| Verb    | local | CI    | result | notes" in out
    assert "→ o/r/rust-cli : pilot-running" in out
    assert "| check" in out


def test_render_table_quiet_hides_pass_rows():
    res = _agg(
        "rust-cli",
        "PASS|ci.yml",
        {
            "check": "FAIL|bin/check missing",
            "build": "PASS|bin/build",
            "release": "PASS|v1 via rust-cli.yml",
            "release:local": "PASS|bin/release",
        },
    )
    out = done_check.render_table(res, quiet=True)
    assert "| check" in out  # the FAIL row stays
    assert "| build" not in out  # PASS rows suppressed
    assert "| release" not in out


# --- main() dispatch (gh + check_* mocked at the data layer) ---------


def _wire_main(monkeypatch, *, stack="rust-cli", ci="PASS|ci.yml"):
    monkeypatch.setattr(done_check, "detect_stack", lambda r: stack)
    monkeypatch.setattr(done_check, "check_ci", lambda r: ci)
    monkeypatch.setattr(done_check, "check_release", lambda r, s: "PASS|v1 via rust-cli.yml")
    monkeypatch.setattr(done_check, "check_local", lambda r, v: f"PASS|bin/{v}")


def test_main_requires_repo_or_detects(monkeypatch, capsys):
    monkeypatch.setattr(done_check, "_detect_current_repo", lambda: "")
    rc = done_check.main([])
    assert rc == 64
    assert "could not detect repo" in capsys.readouterr().err


def test_main_json_pilot_running(monkeypatch, capsys):
    _wire_main(monkeypatch)
    rc = done_check.main(["--repo", "o/r", "--json"])
    out = capsys.readouterr().out
    assert rc == 0
    # last line is the JSON object (first line is "Detecting stack…", etc.)
    obj = json.loads(out.strip().splitlines()[-1])
    assert obj["state"] == "pilot-running"
    assert obj["repo"] == "o/r"


def test_main_human_table(monkeypatch, capsys):
    _wire_main(monkeypatch)
    rc = done_check.main(["--repo", "o/r"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "→ o/r/rust-cli : pilot-running" in out


def test_main_brew_tap_out_of_scope(monkeypatch, capsys):
    monkeypatch.setattr(done_check, "detect_stack", lambda r: "brew-tap")
    rc = done_check.main(["--repo", "o/r"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "out-of-scope" in out


def test_main_brew_tap_json(monkeypatch, capsys):
    monkeypatch.setattr(done_check, "detect_stack", lambda r: "brew-tap")
    rc = done_check.main(["--repo", "o/r", "--json"])
    out = capsys.readouterr().out
    assert rc == 0
    obj = json.loads(out.strip().splitlines()[-1])
    assert obj["state"] == "out-of-scope"
    assert obj["verbs"] == []


def test_main_undetectable_stack_exit_65(monkeypatch, capsys):
    def boom(r):
        raise done_check.StackError("nope")

    monkeypatch.setattr(done_check, "detect_stack", boom)
    rc = done_check.main(["--repo", "o/r"])
    assert rc == 65
    assert "could not detect stack" in capsys.readouterr().err


def test_main_fail_exits_1(monkeypatch, capsys):
    monkeypatch.setattr(done_check, "detect_stack", lambda r: "rust-cli")
    monkeypatch.setattr(done_check, "check_ci", lambda r: "PASS|ci.yml")
    monkeypatch.setattr(done_check, "check_release", lambda r, s: "PASS|v1 via rust-cli.yml")
    monkeypatch.setattr(
        done_check,
        "check_local",
        lambda r, v: "FAIL|bin/check missing" if v == "check" else f"PASS|bin/{v}",
    )
    rc = done_check.main(["--repo", "o/r"])
    assert rc == 1


def test_main_unknown_arg_exits_64(capsys):
    rc = done_check.main(["--nope"])
    assert rc == 64
    assert "unknown arg: --nope" in capsys.readouterr().err


def test_main_repo_without_value_exits_64(capsys):
    rc = done_check.main(["--repo"])
    assert rc == 64


def test_main_help_exits_0(capsys):
    rc = done_check.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out
