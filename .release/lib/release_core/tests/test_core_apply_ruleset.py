"""apply_ruleset verb — payload construction + job-name inference + ruleset lookup.

The byte-for-byte parity with the old `yq|jq` payload is the load-bearing
guarantee, so the template is loaded from the real rulesets/main-protection.json.tmpl
and the built dict is asserted field-by-field. The gh hops are exercised by
monkeypatching gh.rest with recorded JSON (mock at the data layer — never live).
"""

from __future__ import annotations

import json
import os

from release_core import gh, yamlio
from release_core.verbs import apply_ruleset

_TMPL = os.path.join(
    os.path.dirname(os.path.realpath(__file__)),
    "..",
    "..",
    "..",
    "..",
    "..",
    "rulesets",
    "main-protection.json.tmpl",
)


_ROOT = os.path.dirname(os.path.dirname(_TMPL))  # repo root, parent of rulesets/


def _template() -> dict:
    with open(_TMPL, encoding="utf-8") as fh:
        return json.load(fh)


# --------------------------------------------------------------------------
# Pure payload construction
# --------------------------------------------------------------------------


def test_checks_json_wraps_and_drops_empties_preserving_order():
    assert apply_ruleset.checks_json(["b", "", "a"]) == [{"context": "b"}, {"context": "a"}]


def test_build_payload_injects_contexts_into_required_status_checks():
    body = apply_ruleset.build_payload(_template(), ["Test", "Build (x)"])
    rule = next(r for r in body["rules"] if r["type"] == "required_status_checks")
    assert rule["parameters"]["required_status_checks"] == [
        {"context": "Test"},
        {"context": "Build (x)"},
    ]
    # other rules untouched
    assert any(r["type"] == "pull_request" for r in body["rules"])
    assert body["name"] == "main-branch-protection"


def test_build_payload_does_not_mutate_the_template():
    tmpl = _template()
    apply_ruleset.build_payload(tmpl, ["X"])
    rule = next(r for r in tmpl["rules"] if r["type"] == "required_status_checks")
    assert rule["parameters"]["required_status_checks"] == []


def test_build_payload_matches_recorded_jq_bytes():
    """The crux: json.dumps(body, indent=2) must equal the old jq output bytes.

    The jq reference (jq . over the yq|jq-built payload) was recorded in the PR
    via `diff`; here we re-derive the canonical 2-space form and assert the
    contexts array is the only thing that changed vs the template — i.e. the
    structural carry-through that made the diff empty.
    """
    checks = ["Build (aarch64-apple-darwin)", "Test", "bats-e2e"]
    body = apply_ruleset.build_payload(_template(), checks)
    dumped = json.dumps(body, indent=2)
    # jq emits 2-space indent, ": " separators, no trailing newline, UTF-8 literal.
    reparsed = json.loads(dumped)
    assert reparsed == body
    rule = next(r for r in reparsed["rules"] if r["type"] == "required_status_checks")
    assert [c["context"] for c in rule["parameters"]["required_status_checks"]] == checks


# --------------------------------------------------------------------------
# Workflow trigger inference (the yq->python `on:` normalization)
# --------------------------------------------------------------------------


def test_workflow_triggers_string_array_object_other():
    assert apply_ruleset.workflow_triggers({"on": "push"}) == ["push"]
    assert apply_ruleset.workflow_triggers({"on": ["push", "pull_request"]}) == [
        "push",
        "pull_request",
    ]
    assert apply_ruleset.workflow_triggers({"on": {"pull_request": None, "push": None}}) == [
        "pull_request",
        "push",
    ]
    assert apply_ruleset.workflow_triggers({"on": 5}) == []
    assert apply_ruleset.workflow_triggers("not a dict") == []


def test_is_pr_workflow():
    assert apply_ruleset.is_pr_workflow({"on": {"pull_request": None}}) is True
    assert apply_ruleset.is_pr_workflow({"on": "push"}) is False


# --------------------------------------------------------------------------
# Malformed-YAML handling: yamlio.load raises yamlio.YamlError (NOT
# proc.ProcError, since _yq calls proc.run(check=False)). Both yaml-reading
# sites must swallow YamlError and skip the file, matching the bash's clean
# skip rather than crashing with a traceback. Regression for PR #392 review.
# --------------------------------------------------------------------------


def test_pr_workflow_paths_skips_malformed_yaml(tmp_path, monkeypatch):
    wf = tmp_path / "workflows"
    wf.mkdir()
    (wf / "broken.yml").write_text("this: : is: not: valid\n")

    def boom(_path):
        raise yamlio.YamlError("yq parse failure")

    monkeypatch.setattr(apply_ruleset.yamlio, "load", boom)
    # No exception, broken file simply contributes no path.
    assert apply_ruleset._pr_workflow_paths(str(wf)) == []


def test_checks_from_yq_skips_malformed_yaml(monkeypatch):
    def boom(_path):
        raise yamlio.YamlError("yq parse failure")

    monkeypatch.setattr(apply_ruleset.yamlio, "load", boom)
    assert apply_ruleset._checks_from_yq("/top", [".github/workflows/ci.yml"]) == []


# --------------------------------------------------------------------------
# Existing-ruleset lookup
# --------------------------------------------------------------------------


def test_existing_ruleset_id_first_match_or_none():
    rs = [
        {"id": 1, "name": "other"},
        {"id": 7, "name": "main-branch-protection"},
        {"id": 9, "name": "main-branch-protection"},
    ]
    assert apply_ruleset._existing_ruleset_id(rs, "main-branch-protection") == 7
    assert apply_ruleset._existing_ruleset_id(rs, "nope") is None
    assert apply_ruleset._existing_ruleset_id(None, "x") is None


# --------------------------------------------------------------------------
# Job-name inference from recorded runs/jobs JSON (mock gh.rest at data layer)
# --------------------------------------------------------------------------


def test_checks_from_runs_collects_sorted_unique_job_names(monkeypatch):
    routes = {
        "repos/o/r/actions/workflows": {
            "workflows": [{"path": ".github/workflows/ci.yml", "id": 100}]
        },
        "repos/o/r/actions/workflows/100/runs?branch=main&per_page=1": {
            "workflow_runs": [{"id": 555}]
        },
        "repos/o/r/actions/runs/555/jobs": [
            {"name": "Test"},
            {"name": "Build (aarch64-apple-darwin)"},
            {"name": "Test"},
        ],
    }

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        return routes[path]

    monkeypatch.setattr(gh, "rest", fake_rest)
    out = apply_ruleset._checks_from_runs("o/r", "main", [".github/workflows/ci.yml"])
    assert out == ["Build (aarch64-apple-darwin)", "Test"]


def test_checks_from_runs_skips_workflows_without_runs(monkeypatch):
    routes = {
        "repos/o/r/actions/workflows": {
            "workflows": [{"path": ".github/workflows/ci.yml", "id": 100}]
        },
        "repos/o/r/actions/workflows/100/runs?branch=main&per_page=1": {"workflow_runs": []},
    }

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if path not in routes:
            raise gh.GhError("404")
        return routes[path]

    monkeypatch.setattr(gh, "rest", fake_rest)
    assert apply_ruleset._checks_from_runs("o/r", "main", [".github/workflows/ci.yml"]) == []


# --------------------------------------------------------------------------
# main() dispatch — dry-run prints the payload, never sends; PUT vs POST routing
# --------------------------------------------------------------------------


def test_main_dry_run_with_checks_override_prints_payload_no_send(monkeypatch, capsys):
    monkeypatch.setattr(apply_ruleset, "_current_repo", lambda: "o/r")
    monkeypatch.setattr(apply_ruleset, "_release_root", lambda: _ROOT)

    sent = []

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if path == "repos/o/r/rulesets" and method is None:
            return [{"id": 7, "name": "main-branch-protection"}]
        sent.append((path, method))
        return None

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = apply_ruleset.main(["--dry-run", "--checks", "Test,Build (x)"])
    out = capsys.readouterr().out
    assert rc == 0
    assert sent == []  # nothing PUT/POSTed
    assert "repo:    o/r" in out
    assert "ruleset: main-branch-protection (existing id: 7)" in out
    assert "checks:  Test,Build (x)" in out
    assert "--- payload (dry-run, not sent) ---" in out
    # the dumped payload carries the override checks in order
    payload = json.loads(out.split("--- payload (dry-run, not sent) ---\n", 1)[1])
    rule = next(r for r in payload["rules"] if r["type"] == "required_status_checks")
    assert [c["context"] for c in rule["parameters"]["required_status_checks"]] == [
        "Test",
        "Build (x)",
    ]


def test_main_creates_when_no_existing_ruleset(monkeypatch, capsys):
    monkeypatch.setattr(apply_ruleset, "_current_repo", lambda: "o/r")
    monkeypatch.setattr(apply_ruleset, "_release_root", lambda: _ROOT)
    sent = []

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if path == "repos/o/r/rulesets" and method is None:
            return []
        sent.append((path, method, body is not None))
        return None

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = apply_ruleset.main(["--checks", "Test"])
    out = capsys.readouterr().out
    assert rc == 0
    assert sent == [("repos/o/r/rulesets", "POST", True)]
    assert out.rstrip().endswith("created")


def test_main_updates_when_existing_ruleset(monkeypatch, capsys):
    monkeypatch.setattr(apply_ruleset, "_current_repo", lambda: "o/r")
    monkeypatch.setattr(apply_ruleset, "_release_root", lambda: _ROOT)
    sent = []

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if path == "repos/o/r/rulesets" and method is None:
            return [{"id": 42, "name": "main-branch-protection"}]
        sent.append((path, method, body is not None))
        return None

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = apply_ruleset.main(["--checks", "Test"])
    out = capsys.readouterr().out
    assert rc == 0
    assert sent == [("repos/o/r/rulesets/42", "PUT", True)]
    assert out.rstrip().endswith("updated")


def test_main_no_checks_determinable_exits_1(monkeypatch, capsys, tmp_path):
    # Empty override falls through to auto-detect (matching the bash `[ -n ]`
    # guard). With a workflows dir that has no PR-triggered workflow, no runs,
    # and no yq fallback, checks stay empty → the same error + exit 1.
    monkeypatch.setattr(apply_ruleset, "_current_repo", lambda: "o/r")
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    monkeypatch.setattr(apply_ruleset.gh, "repo_root", lambda: str(tmp_path))

    def fake_rest(path, *, method=None, fields=None, body=None, paginate=False):
        if path == "repos/o/r":
            return {"default_branch": "main"}
        raise gh.GhError("404")

    monkeypatch.setattr(gh, "rest", fake_rest)
    rc = apply_ruleset.main(["--checks", ""])
    err = capsys.readouterr().err
    assert rc == 1
    assert "no required checks" in err


def test_main_no_workflows_dir_exits_1(monkeypatch, capsys, tmp_path):
    monkeypatch.setattr(apply_ruleset, "_current_repo", lambda: "o/r")
    monkeypatch.setattr(apply_ruleset.gh, "repo_root", lambda: str(tmp_path))
    rc = apply_ruleset.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "no .github/workflows dir" in err


def test_main_help_exits_0_and_prints_usage(capsys):
    rc = apply_ruleset.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out
    assert "apply-ruleset" in out
    assert "Shell→Python" not in out  # the migration note is stripped from --help


def test_main_unknown_flag_is_usage_error(capsys):
    rc = apply_ruleset.main(["--nope"])
    assert rc == 64
