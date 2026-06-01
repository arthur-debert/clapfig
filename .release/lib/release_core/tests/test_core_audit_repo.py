"""audit_repo verb — pure decision helpers + check dispatch over a mock gh layer.

The YAML/workflow/changelog parsing oracles are tested directly against recorded
strings. The checks are exercised by monkeypatching gh.rest with a routing dict
of recorded JSON responses (mock at the data layer — never subprocess), asserting
the (status, name, message) rows, the --json shape, and the exit-code precedence.
"""

from __future__ import annotations

import base64
import json

from release_core import gh
from release_core.verbs import audit_repo

# --------------------------------------------------------------------------
# Pure helpers
# --------------------------------------------------------------------------


def test_off_policy_ecosystems_github_actions_only():
    yml = "version: 2\nupdates:\n  - package-ecosystem: github-actions\n    directory: /\n"
    assert audit_repo.off_policy_ecosystems(yml) == []


def test_off_policy_ecosystems_finds_app_deps_sorted_unique():
    yml = (
        "updates:\n"
        "  - package-ecosystem: npm\n"
        "  - package-ecosystem: github-actions\n"
        '  - package-ecosystem: "cargo"\n'
        "  - package-ecosystem: npm\n"
    )
    assert audit_repo.off_policy_ecosystems(yml) == ["cargo", "npm"]


def test_has_private_go_deps_true_on_require_block():
    gomod = (
        "module github.com/arthur-debert/thisrepo\n\n"
        "go 1.22\n\n"
        "require (\n"
        "\tgithub.com/arthur-debert/secretlib v1.2.3\n"
        ")\n"
    )
    assert audit_repo.has_private_go_deps(gomod) is True


def test_has_private_go_deps_ignores_own_module_line():
    gomod = "module github.com/arthur-debert/thisrepo\n\ngo 1.22\n"
    assert audit_repo.has_private_go_deps(gomod) is False


def test_has_private_go_deps_false_without_arthur_debert():
    gomod = "module example.com/x\n\nrequire github.com/spf13/cobra v1.0.0\n"
    assert audit_repo.has_private_go_deps(gomod) is False


def test_workflow_has_private_auth_needs_both_signals():
    # The real pattern: `...insteadOf "https://github.com/"` — insteadOf precedes
    # a github.com, matching the bash grep 'insteadOf.*github\.com'.
    both = 'git config url.x.insteadOf "https://github.com/" ... ${{ secrets.RELEASE_TOKEN }}'
    assert audit_repo.workflow_has_private_auth(both) is True
    assert audit_repo.workflow_has_private_auth('insteadOf "github.com"') is False
    assert audit_repo.workflow_has_private_auth("RELEASE_TOKEN only") is False


def test_classify_copilot_pointer():
    rel = "uses: arthur-debert/release/.github/workflows/copilot-review.yml@v1"
    old = "uses: arthur-debert/gh-dagentic/.github/workflows/copilot-review.yml@main"
    assert audit_repo.classify_copilot_pointer(rel) == "release"
    assert audit_repo.classify_copilot_pointer(old) == "gh-dagentic"
    assert audit_repo.classify_copilot_pointer("uses: some/other@v1") == "unknown"


def test_classify_changelog_precedence():
    assert audit_repo.classify_changelog(True, False, False) == (
        "PASS",
        "fragment-dir (canonical)",
    )
    assert audit_repo.classify_changelog(False, True, False)[0] == "WARN"
    assert audit_repo.classify_changelog(False, False, True)[0] == "WARN"
    assert audit_repo.classify_changelog(False, False, False)[0] == "WARN"
    status, msg = audit_repo.classify_changelog(True, True, False)
    assert status == "FAIL"
    assert "mixed" in msg


def test_parse_release_sync_state():
    body = "sha: abcdef1234567890\ncomponents:\n  - rust-cli\n  - bats\nother:\n  - ignored\n"
    sha, comps = audit_repo.parse_release_sync_state(body)
    assert sha == "abcdef1234567890"
    assert comps == "rust-cli,bats"


def test_exit_code_precedence():
    assert audit_repo.exit_code([("PASS", "a", "")]) == 0
    assert audit_repo.exit_code([("PASS", "a", ""), ("WARN", "b", "")]) == 2
    assert audit_repo.exit_code([("WARN", "b", ""), ("FAIL", "c", "")]) == 1


# --------------------------------------------------------------------------
# Check dispatch over a routed gh.rest mock
# --------------------------------------------------------------------------


def _b64(text: str) -> dict:
    return {"content": base64.b64encode(text.encode()).decode()}


class _GhRouter:
    """Routes gh.rest(path) → a recorded response, raising GhError to emulate a 404."""

    def __init__(self, routes: dict):
        self.routes = routes

    def __call__(self, path, **kw):
        # strip query strings to match by base path when convenient
        if path in self.routes:
            val = self.routes[path]
        else:
            val = self.routes.get(path.split("?")[0], _NOT_FOUND)
        if val is _NOT_FOUND:
            raise gh.GhError(f"gh api {path} failed (1): gh: Not Found (HTTP 404)")
        if isinstance(val, gh.GhError):
            raise val
        return val


_NOT_FOUND = object()


def _green_routes(repo="o/r"):
    """A fully-conformant repo: every check PASS where possible."""
    return {
        f"repos/{repo}/rulesets": [{"id": 99, "name": "main-branch-protection"}],
        f"repos/{repo}/actions/secrets": {"secrets": [{"name": "RELEASE_TOKEN"}]},
        f"repos/{repo}/contents/.github/workflows/copilot-review.yml": _b64(
            "uses: arthur-debert/release/.github/workflows/copilot-review.yml@v1\n"
        ),
        f"repos/{repo}/contents/.github/CODEOWNERS": _b64("* @arthur-debert"),
        f"repos/{repo}/vulnerability-alerts": None,  # 204 → enabled
        f"repos/{repo}/contents/.github/dependabot.yml": _b64(
            "updates:\n  - package-ecosystem: github-actions\n"
        ),
        f"repos/{repo}": {"default_branch": "main"},
        f"repos/{repo}/actions/runs": {"workflow_runs": [{"name": "CI", "conclusion": "success"}]},
        # no go.mod → SKIP private_mod_auth
        f"repos/{repo}/contents/.release-sync-state.yaml": _b64(
            "sha: deadbeefcafe\ncomponents:\n  - rust-cli\n"
        ),
        f"repos/{repo}/contents/scripts": [{"type": "file", "name": "setup-dev-env.sh"}],
        f"repos/{repo}/contents/.github/workflows": [
            {"type": "file", "name": "ci.yml"},
            {"type": "file", "name": "copilot-review.yml"},
        ],
        f"repos/{repo}/contents/.github/workflows/ci.yml": _b64(
            "jobs:\n  t:\n    uses: arthur-debert/release/.github/workflows/rust-ci.yml@v1\n"
        ),
        f"repos/{repo}/contents/CHANGELOG": [{"type": "file", "name": "1.md"}],
    }


def _rows(monkeypatch, routes, repo="o/r"):
    monkeypatch.setattr(gh, "rest", _GhRouter(routes))
    return {name: (status, msg) for status, name, msg in audit_repo.audit(repo)}


def test_audit_all_green(monkeypatch):
    rows = _rows(monkeypatch, _green_routes())
    assert rows["ruleset"][0] == "PASS"
    assert rows["release_token"][0] == "PASS"
    assert rows["copilot_review"][0] == "PASS"
    assert rows["codeowners"][0] == "PASS"
    assert rows["dep_security"][0] == "PASS"
    assert rows["dep_policy"][0] == "PASS"
    assert rows["ci_main_green"][0] == "PASS"
    assert rows["private_mod_auth"][0] == "SKIP"  # no go.mod
    assert rows["release_sync"][0] == "PASS"
    assert rows["scripts_inventory"][0] == "PASS"
    # ci.yml uses rust-ci reusable → workflows_canonical PASS, ci_calls_bin_check PASS via reusable
    assert rows["workflows_canonical"][0] == "PASS"
    assert rows["ci_calls_bin_check"][0] == "PASS"
    assert rows["changelog_handling"][0] == "PASS"


def test_dep_security_404_is_fail(monkeypatch):
    routes = _green_routes()
    del routes["repos/o/r/vulnerability-alerts"]  # absent → router raises 404 → FAIL
    rows = _rows(monkeypatch, routes)
    assert rows["dep_security"][0] == "FAIL"
    assert "disabled" in rows["dep_security"][1]


def test_dep_policy_off_policy_fails(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/.github/dependabot.yml"] = _b64(
        "updates:\n  - package-ecosystem: npm\n  - package-ecosystem: github-actions\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["dep_policy"][0] == "FAIL"
    assert "npm" in rows["dep_policy"][1]


def test_ruleset_missing_fails(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/rulesets"] = [{"id": 1, "name": "something-else"}]
    rows = _rows(monkeypatch, routes)
    assert rows["ruleset"][0] == "FAIL"


def test_copilot_gh_dagentic_pointer_fails(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/.github/workflows/copilot-review.yml"] = _b64(
        "uses: arthur-debert/gh-dagentic/.github/workflows/copilot-review.yml@main\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["copilot_review"][0] == "FAIL"
    assert "gh-dagentic" in rows["copilot_review"][1]


def test_private_mod_auth_pass_when_workflow_has_auth(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/go.mod"] = _b64(
        "module github.com/arthur-debert/me\nrequire github.com/arthur-debert/dep v1.0.0\n"
    )
    routes["repos/o/r/contents/.github/workflows/ci.yml"] = _b64(
        'run: git config url.x.insteadOf "github.com" ... ${{ secrets.RELEASE_TOKEN }}\n'
    )
    rows = _rows(monkeypatch, routes)
    assert rows["private_mod_auth"][0] == "PASS"


def test_private_mod_auth_fail_when_no_auth(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/go.mod"] = _b64(
        "module github.com/arthur-debert/me\nrequire github.com/arthur-debert/dep v1.0.0\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["private_mod_auth"][0] == "FAIL"


def test_ci_main_green_failure(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/actions/runs"] = {"workflow_runs": [{"name": "CI", "conclusion": "failure"}]}
    rows = _rows(monkeypatch, routes)
    assert rows["ci_main_green"][0] == "FAIL"


def test_ci_main_green_skips_copilot_run(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/actions/runs"] = {
        "workflow_runs": [
            {"name": "Copilot Review", "conclusion": "failure"},
            {"name": "CI", "conclusion": "success"},
        ]
    }
    rows = _rows(monkeypatch, routes)
    assert rows["ci_main_green"][0] == "PASS"


def test_workflows_bespoke_warns(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/.github/workflows"] = [{"type": "file", "name": "bespoke.yml"}]
    routes["repos/o/r/contents/.github/workflows/bespoke.yml"] = _b64(
        "jobs:\n  x:\n    runs-on: ubuntu\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["workflows_canonical"][0] == "WARN"
    assert "bespoke.yml" in rows["workflows_canonical"][1]


def test_ci_calls_bin_check_direct(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/.github/workflows/ci.yml"] = _b64(
        "steps:\n  - name: check\n    run: bin/check\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["ci_calls_bin_check"][0] == "PASS"
    assert "called in: ci.yml" in rows["ci_calls_bin_check"][1]


def test_ci_calls_bin_check_warns_when_absent(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/.github/workflows/ci.yml"] = _b64(
        "jobs:\n  x:\n    runs-on: ubuntu\n"
    )
    rows = _rows(monkeypatch, routes)
    assert rows["ci_calls_bin_check"][0] == "WARN"


def test_changelog_mixed_fails(monkeypatch):
    routes = _green_routes()
    routes["repos/o/r/contents/CHANGELOG.md"] = _b64("## Unreleased\n- thing\n")
    routes["repos/o/r/contents/CHANGELOG_UNRELEASED.md"] = _b64("- x")
    rows = _rows(monkeypatch, routes)
    assert rows["changelog_handling"][0] == "FAIL"


def test_changelog_single_file_warns(monkeypatch):
    routes = _green_routes()
    del routes["repos/o/r/contents/CHANGELOG"]  # no fragment dir
    routes["repos/o/r/contents/CHANGELOG.md"] = _b64("## [Unreleased]\n- x\n")
    rows = _rows(monkeypatch, routes)
    assert rows["changelog_handling"][0] == "WARN"
    assert "single-file" in rows["changelog_handling"][1]


# --------------------------------------------------------------------------
# main() — output shapes + exit codes
# --------------------------------------------------------------------------


def test_main_json_shape(monkeypatch, capsys):
    monkeypatch.setattr(gh, "rest", _GhRouter(_green_routes()))
    rc = audit_repo.main(["--repo", "o/r", "--json"])
    assert rc == 0
    data = json.loads(capsys.readouterr().out)
    assert data["repo"] == "o/r"
    names = {c["name"] for c in data["checks"]}
    assert "ruleset" in names and "changelog_handling" in names
    assert all({"name", "status", "message"} == set(c) for c in data["checks"])


def test_main_human_quiet_hides_pass_and_skip(monkeypatch, capsys):
    routes = _green_routes()
    routes["repos/o/r/rulesets"] = [{"id": 1, "name": "nope"}]  # one FAIL
    monkeypatch.setattr(gh, "rest", _GhRouter(routes))
    rc = audit_repo.main(["--repo", "o/r", "--quiet"])
    out = capsys.readouterr().out
    assert rc == 1  # a FAIL present
    assert "[FAIL]" in out
    assert "[PASS]" not in out
    assert "[SKIP]" not in out


def test_main_unknown_arg_exits_64(capsys):
    rc = audit_repo.main(["--nope"])
    assert rc == 64


def test_main_repo_without_value_exits_64(capsys):
    rc = audit_repo.main(["--repo"])
    assert rc == 64


def test_main_help_exits_0(capsys):
    rc = audit_repo.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out


def test_main_no_repo_and_no_gh_repo_exits_64(monkeypatch, capsys):
    monkeypatch.setattr(audit_repo, "_current_repo", lambda: "")
    rc = audit_repo.main([])
    assert rc == 64
    assert "not in a gh-recognized repo" in capsys.readouterr().err
