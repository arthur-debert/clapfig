"""gh_release_issue verb — context collection, title/body assembly, label, and
the stdout URL. Pure functions tested directly; main()'s gh boundary
(collect_context / lookup_run_url / _create_issue) monkeypatched at the data
layer — never the network.
"""

from __future__ import annotations

from release_core.verbs import gh_release_issue as gri

CTX_WITH_PR = {
    "repo": "arthur-debert/dodot",
    "branch": "feat/x",
    "pr_number": "118",
    "pr_url": "https://github.com/arthur-debert/dodot/pull/118",
}

CTX_NO_PR = {
    "repo": "arthur-debert/dodot",
    "branch": "feat/x",
    "pr_number": "",
    "pr_url": "",
}


# --- pure assembly ---


def test_build_title():
    assert gri.build_title("copilot-review", "reviewer empty") == "[copilot-review] reviewer empty"


def test_build_body_includes_pr_and_run_when_present():
    body = gri.build_body("copilot-review", "reviewer empty", CTX_WITH_PR, "https://x/run/1")
    assert "**Component:** copilot-review" in body
    assert "**Reported from:** arthur-debert/dodot" in body
    assert "**Branch:** feat/x" in body
    assert "**PR:** https://github.com/arthur-debert/dodot/pull/118" in body
    assert "**Workflow run:** https://x/run/1" in body
    assert "**Symptom:** reviewer empty" in body
    assert "Filed via `gh-release-issue`" in body


def test_build_body_omits_pr_and_run_lines_when_empty():
    body = gri.build_body("other", "boom", CTX_NO_PR, "")
    assert "**PR:**" not in body
    assert "**Workflow run:**" not in body
    # the line collapses entirely — Branch is immediately followed by the blank
    # line + Symptom block, no stray empty PR/run line.
    assert "**Branch:** feat/x\n\n**Symptom:** boom" in body


def _record_gh(gh_mod, monkeypatch, stdout="https://x/run"):
    """Capture the full `gh …` argv the chokepoint builds (mock at proc.run)."""
    import subprocess

    calls: list = []

    def fake_run(cmd, *, input=None, check=False):  # noqa: A002 — mirrors proc.run
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, stdout=stdout, stderr="")

    monkeypatch.setattr(gh_mod.shutil, "which", lambda _: "/usr/bin/gh")
    monkeypatch.setattr(gh_mod.proc, "run", fake_run)
    return calls


def test_lookup_run_url_only_for_mapped_components(monkeypatch):
    from release_core import gh

    calls = _record_gh(gh, monkeypatch, stdout="https://x/run")
    # mapped → probes; argv is byte-identical to the former direct gh call
    assert gri.lookup_run_url("copilot-review", "feat/x") == "https://x/run"
    assert calls and calls[-1] == [
        "gh",
        "run",
        "list",
        "--workflow",
        "copilot-review.yml",
        "--branch",
        "feat/x",
        "--limit",
        "1",
        "--json",
        "url",
        "-q",
        ".[0].url // empty",
    ]
    # unmapped → no probe, empty
    calls.clear()
    assert gri.lookup_run_url("ruleset", "feat/x") == ""
    assert calls == []


def test_lookup_run_url_unknown_branch_no_branch_filter(monkeypatch):
    from release_core import gh

    calls = _record_gh(gh, monkeypatch, stdout="")
    gri.lookup_run_url("rust-cli-release", "(unknown)")
    assert "--branch" not in calls[0]
    assert calls[0] == [
        "gh",
        "run",
        "list",
        "--workflow",
        "release.yml",
        "--limit",
        "1",
        "--json",
        "url",
        "-q",
        ".[0].url // empty",
    ]


# --- main() dispatch / usage ---


def test_no_args_exits_64(capsys):
    assert gri.main([]) == 64
    assert "Usage:" in capsys.readouterr().err


def test_empty_first_arg_exits_64():
    assert gri.main([""]) == 64


def test_one_arg_exits_64(capsys):
    assert gri.main(["copilot-review"]) == 64
    assert "need <component> <symptom>" in capsys.readouterr().err


def test_help_exits_0(capsys):
    assert gri.main(["--help"]) == 0
    assert "Usage:" in capsys.readouterr().out


def test_main_files_issue_and_prints_url(monkeypatch, capsys):
    monkeypatch.setattr(gri, "collect_context", lambda: dict(CTX_WITH_PR))
    monkeypatch.setattr(gri, "lookup_run_url", lambda c, b: "https://x/run/1")
    captured: dict = {}

    def fake_create(title, body):
        captured["title"] = title
        captured["body"] = body
        return "https://github.com/arthur-debert/release/issues/99"

    monkeypatch.setattr(gri, "_create_issue", fake_create)
    rc = gri.main(["copilot-review", "reviewer empty after success"])
    assert rc == 0
    assert capsys.readouterr().out.strip() == "https://github.com/arthur-debert/release/issues/99"
    assert captured["title"] == "[copilot-review] reviewer empty after success"
    assert "**Workflow run:** https://x/run/1" in captured["body"]


def test_multiword_symptom_captured_whole(monkeypatch, capsys):
    monkeypatch.setattr(gri, "collect_context", lambda: dict(CTX_NO_PR))
    monkeypatch.setattr(gri, "lookup_run_url", lambda c, b: "")
    captured: dict = {}
    monkeypatch.setattr(
        gri, "_create_issue", lambda title, body: captured.update(title=title, body=body) or "url"
    )
    gri.main(["other", "this", "is", "a", "multi", "word", "symptom"])
    assert captured["title"] == "[other] this is a multi word symptom"
    assert "**Symptom:** this is a multi word symptom" in captured["body"]
