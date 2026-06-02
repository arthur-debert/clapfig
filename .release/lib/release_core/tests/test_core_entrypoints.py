"""Console-script entry points (PR-B, pip-bootstrap-contract.md §1).

Guards the three invariants of the [project.scripts] → entrypoints.py seam:

1. Every command in pyproject's [project.scripts] resolves to a real,
   zero-arg, callable wrapper in release_core.entrypoints.
2. Each wrapper delegates to its verb's main with sys.argv[1:] and propagates
   the verb's return code out as the SystemExit code (the console-script
   contract: a wrapper raises SystemExit(<int>)).
3. The script table covers EXACTLY the release_core-backed bin/ shims — no
   more, no less. The expected set below is derived by hand from the bin/
   shims that dispatch to `release_core.verbs.*` (see EXPECTED_COMMANDS); bash
   tools (fetch-deps/fetch-artifact/gh-*/clone-*/migrate-*) and the
   release_gh-backed gh-task-status are intentionally excluded.

Plus a byte-identity sanity check: `detect-kind --help` through the wrapper ==
the verb's own --help output (the wrappers only forward argv, so this holds).
"""

from __future__ import annotations

import inspect
import io
import sys
import tomllib
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import pytest
from release_core import entrypoints
from release_core.verbs import changelog, detect_kind

# Authoritative set of on-PATH command names, derived by reading every
# bin/<name> shim that dispatches to release_core.verbs. Keep this set in
# lockstep with pyproject's [project.scripts] (the test below enforces ==).
EXPECTED_COMMANDS = {
    "apply-ruleset",
    "audit-portfolio",
    "audit-repo",
    "audit-smoke-test",
    "changelog",
    "changelog-add",
    "changelog-cut",
    "changelog-render",
    "detect-kind",
    "done-check",
    "enable-dependabot-security",
    "gh-release-issue",
    "install-release-secrets",
    "install-release-token",
    "list-repo-pr",
    "list-repo-scripts",
    "managed-repos",
    "release-advance-major",
    "release-beta-list",
    "release-cut",
    "release-drift-check",
    "release-inbox",
    "release-lex",
    "release-notify-source",
    "release-sync",
    "release-verify-fleet",
    "semver",
    "sweep-github-policy",
}

# The top-level `release-core` CLI is its own command (the `init` subcommand
# dispatcher, pip-bootstrap PoC §2). Unlike the per-verb scripts it does NOT map
# into release_core.entrypoints — it points straight at the CLI's main.
RELEASE_CORE_CLI = "release-core"
RELEASE_CORE_CLI_TARGET = "release_core.cli_entry:main"


def _pyproject_path() -> Path:
    # tests/ -> release_core/ (package root holding pyproject.toml)
    return Path(__file__).resolve().parent.parent / "pyproject.toml"


def _script_table() -> dict[str, str]:
    with _pyproject_path().open("rb") as fh:
        data = tomllib.load(fh)
    return data["project"]["scripts"]


def test_script_table_matches_expected_command_set():
    """The [project.scripts] keys == the release_core-backed bin/ shims plus the
    top-level `release-core` CLI."""
    assert set(_script_table()) == EXPECTED_COMMANDS | {RELEASE_CORE_CLI}


def test_release_core_cli_registered_at_cli_entry_main():
    """`release-core` is registered as the top-level CLI (cli_entry:main), so a
    single wheel install puts `release-core init` on PATH."""
    assert _script_table().get(RELEASE_CORE_CLI) == RELEASE_CORE_CLI_TARGET


def test_every_target_is_release_core_entrypoints_wrapper():
    """Every per-verb script target points at a real zero-arg callable in
    entrypoints. The top-level `release-core` CLI is the one exception — it maps
    to cli_entry:main, checked separately above."""
    for cmd, target in _script_table().items():
        if cmd == RELEASE_CORE_CLI:
            continue
        module, _, func = target.partition(":")
        assert module == "release_core.entrypoints", (cmd, target)
        wrapper = getattr(entrypoints, func, None)
        assert callable(wrapper), f"{cmd}: missing wrapper {func}"
        # The console-script protocol invokes the wrapper with no arguments, so
        # it must be callable with an empty signature. signature().bind() is the
        # faithful check: it rejects required positional/keyword-only params and
        # is not fooled by *args/**kwargs the way co_argcount == 0 would be.
        try:
            inspect.signature(wrapper).bind()
        except TypeError as exc:  # pragma: no cover - failure path
            pytest.fail(f"{func} is not callable with zero arguments: {exc}")


@pytest.mark.parametrize("cmd", sorted(EXPECTED_COMMANDS))
def test_wrapper_delegates_with_argv_and_propagates_exit_code(cmd, monkeypatch):
    """Each wrapper calls its verb's main(sys.argv[1:]) and raises SystemExit(rc)."""
    target = _script_table()[cmd]
    func = target.split(":", 1)[1]
    wrapper = getattr(entrypoints, func)

    # Discover which verb function this wrapper delegates to by inspecting the
    # bytecode's referenced globals would be brittle; instead drive it: stub
    # sys.argv, monkeypatch the verb fn the wrapper closes over, assert the call.
    captured = {}

    def fake(argv):
        captured["argv"] = argv
        return 7  # arbitrary non-zero sentinel to prove the code is propagated

    # The wrapper resolves its verb fn from the module-level imports in
    # entrypoints. Patch on the verb module so the wrapper picks up the stub.
    # changelog-family map to changelog.{orchestrator,add,cut,render}_main;
    # everything else to <verb_module>.main.
    changelog_funcs = {
        "changelog": "orchestrator_main",
        "changelog-add": "add_main",
        "changelog-cut": "cut_main",
        "changelog-render": "render_main",
    }
    if cmd in changelog_funcs:
        monkeypatch.setattr(changelog, changelog_funcs[cmd], fake)
    else:
        verb_mod_name = cmd.replace("-", "_")
        verb_mod = getattr(
            __import__("release_core.verbs", fromlist=[verb_mod_name]),
            verb_mod_name,
        )
        monkeypatch.setattr(verb_mod, "main", fake)

    monkeypatch.setattr(sys, "argv", [cmd, "--flag", "value"])
    with pytest.raises(SystemExit) as exc:
        wrapper()

    assert captured["argv"] == ["--flag", "value"]
    assert exc.value.code == 7


def test_help_byte_identical_to_verb(monkeypatch):
    """Sanity: detect-kind --help via the wrapper == the verb's own --help."""
    # Verb directly. Capture both streams: --help could in principle land on
    # stderr, and we want byte-identity across the full output, not just stdout.
    direct_out, direct_err = io.StringIO(), io.StringIO()
    with redirect_stdout(direct_out), redirect_stderr(direct_err):
        rc_direct = detect_kind.main(["--help"])

    # Via the console-script wrapper (it forwards sys.argv[1:]).
    monkeypatch.setattr(sys, "argv", ["detect-kind", "--help"])
    wrapped_out, wrapped_err = io.StringIO(), io.StringIO()
    with (
        redirect_stdout(wrapped_out),
        redirect_stderr(wrapped_err),
        pytest.raises(SystemExit) as exc,
    ):
        entrypoints.detect_kind_main()

    assert wrapped_out.getvalue() == direct_out.getvalue()
    assert wrapped_err.getvalue() == direct_err.getvalue()
    assert exc.value.code == rc_direct == 0
