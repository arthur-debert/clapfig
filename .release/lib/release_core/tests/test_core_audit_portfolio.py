"""audit_portfolio verb — aggregation over the audit_repo verb.

audit_repo.audit is monkeypatched to return canned per-repo rows (the data
layer), so the table/conformance/exit-code aggregation is tested offline. The
manifest read uses a temp YAML file via the real yamlio (yq), exercising the
fleet-list path without network.
"""

from __future__ import annotations

import json

from release_core.verbs import audit_portfolio, audit_repo


def test_conformance_pct_excludes_skips():
    assert audit_portfolio.conformance_pct(8, 0, 0) == 100
    assert audit_portfolio.conformance_pct(3, 1, 0) == 75
    assert audit_portfolio.conformance_pct(0, 0, 0) == 0  # all skipped


# Canned per-repo audit results keyed by repo.
_FIXTURE = {
    "o/green": [("PASS", "a", ""), ("PASS", "b", ""), ("SKIP", "c", "")],
    "o/warn": [("PASS", "a", ""), ("WARN", "b", "soft")],
    "o/fail": [("PASS", "a", ""), ("FAIL", "b", "hard"), ("WARN", "c", "soft")],
}


def _patch_audit(monkeypatch):
    monkeypatch.setattr(audit_repo, "audit", lambda r: list(_FIXTURE[r]))


def test_json_mode_one_object_per_line(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    rc = audit_portfolio.main(["--repos", "o/green,o/warn,o/fail", "--json"])
    assert rc == 0
    lines = [ln for ln in capsys.readouterr().out.splitlines() if ln.strip()]
    assert len(lines) == 3
    first = json.loads(lines[0])
    assert first["repo"] == "o/green"
    assert {"name", "status", "message"} == set(first["checks"][0])


def test_table_exit_code_fail_dominates(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    rc = audit_portfolio.main(["--repos", "o/green,o/warn,o/fail"])
    out = capsys.readouterr().out
    assert rc == 1  # a FAIL present anywhere
    assert "REPO" in out and "STATUS" in out
    assert "o/fail" in out
    assert "summary:" in out


def test_table_exit_code_warn_when_no_fail(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    rc = audit_portfolio.main(["--repos", "o/green,o/warn"])
    assert rc == 2


def test_table_exit_code_green(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    rc = audit_portfolio.main(["--repos", "o/green"])
    assert rc == 0


def test_only_failing_hides_green_rows(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    audit_portfolio.main(["--repos", "o/green,o/fail", "--only-failing"])
    out = capsys.readouterr().out
    # The green repo is hidden from the table body; the failing one shown.
    table_section = out.split("=== details", 1)[0]
    assert "o/green" not in table_section
    assert "o/fail" in table_section


def test_details_section_for_problem_repos(monkeypatch, capsys):
    _patch_audit(monkeypatch)
    audit_portfolio.main(["--repos", "o/green,o/fail"])
    out = capsys.readouterr().out
    assert "=== details for repos with failures or warnings ===" in out
    # quiet detail rows drop PASS/SKIP; the FAIL row's message surfaces.
    assert "hard" in out


def test_unknown_arg_exits_64(capsys):
    rc = audit_portfolio.main(["--nope"])
    assert rc == 64


def test_help_exits_0(capsys):
    rc = audit_portfolio.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out


def test_empty_manifest_exits_1(monkeypatch, tmp_path, capsys):
    # An empty fleet (no projects) → "no onboarded repos", exit 1 (matches bash).
    manifest = tmp_path / "managed-repos.yaml"
    manifest.write_text("projects: {}\n")
    monkeypatch.setattr(audit_portfolio, "_manifest_path", lambda: str(manifest))
    rc = audit_portfolio.main([])
    assert rc == 1
    assert "no onboarded repos" in capsys.readouterr().err


def test_manifest_read_path(monkeypatch, tmp_path, capsys):
    manifest = tmp_path / "managed-repos.yaml"
    manifest.write_text(
        "projects:\n"
        "  lex:\n"
        "    - { repo: lex-fmt/lex, path: lex-fmt/lex }\n"
        "  dodot:\n"
        "    - { repo: arthur-debert/dodot, path: dodot }\n"
    )
    monkeypatch.setattr(audit_portfolio, "_manifest_path", lambda: str(manifest))
    monkeypatch.setattr(audit_repo, "audit", lambda r: [("PASS", "a", "")])
    rc = audit_portfolio.main([])
    assert rc == 0
    out = capsys.readouterr().out
    assert "lex-fmt/lex" in out
    assert "arthur-debert/dodot" in out
