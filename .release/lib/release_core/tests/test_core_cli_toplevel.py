"""``release-core`` top-level per-project commands + the ``ci`` group (release#460).

Covers the surface this group's agent filled in:

  * flat verb-wraps: ``detect-kind``, ``audit``, ``semver`` (semver self-dispatches
    its own validate/get, so it is a single passthrough leaf, NOT a group);
  * the ``changelog`` group (bare → orchestrator; ``add``/``cut``/``render`` leaves);
  * the ``sync`` group (bare/``run`` → release-sync; ``drift-check`` → drift gate);
  * the ``issue`` group (``issue file`` → gh-release-issue);
  * the ``ci`` group (``fetch-deps`` / ``fetch-artifact`` script-wraps).

The assertions exercise DISPATCH (argv reaches the right verb's own ``--help`` /
usage, byte-faithfully through the passthrough) and REGISTRATION/short_help for
the script-backed leaves (which we don't actually exec — they shell out to gh).
"""

from __future__ import annotations

import click
from release_core import cli_entry
from release_core.cli import ci, toplevel


def _attached_root() -> click.Group:
    return cli_entry.root


# --- registration / shape -------------------------------------------------


def test_flat_commands_registered():
    names = set(_attached_root().commands)
    assert {"detect-kind", "audit", "semver"} <= names


def test_semver_is_a_flat_leaf_not_a_group():
    # semver dispatches its own validate/get positional verbs; it must stay a
    # single passthrough command so argv reaches semver.main intact.
    cmd = _attached_root().commands["semver"]
    assert not isinstance(cmd, click.Group)
    assert (cmd.short_help or "").strip()


def test_changelog_group_has_add_cut_render():
    grp = _attached_root().commands["changelog"]
    assert isinstance(grp, click.Group)
    assert set(grp.commands) >= {"add", "cut", "render"}


def test_sync_group_has_run_and_drift_check():
    grp = _attached_root().commands["sync"]
    assert isinstance(grp, click.Group)
    assert set(grp.commands) >= {"run", "drift-check"}


def test_issue_group_has_file():
    grp = _attached_root().commands["issue"]
    assert isinstance(grp, click.Group)
    assert "file" in grp.commands


def test_ci_group_has_fetch_leaves():
    assert isinstance(ci.group, click.Group)
    assert set(ci.group.commands) >= {"fetch-deps", "fetch-artifact"}


def test_ci_leaves_have_short_help():
    for name in ("fetch-deps", "fetch-artifact"):
        leaf = ci.group.commands[name]
        assert (leaf.short_help or "").strip(), name


# --- dispatch: argv reaches the underlying verb (byte-faithful passthrough) -


def test_detect_kind_dispatches_to_verb_help(capsys):
    rc = cli_entry.main(["detect-kind", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "detect-kind" in text


def test_audit_dispatches_to_verb_help(capsys):
    rc = cli_entry.main(["audit", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "audit-repo" in text


def test_semver_passthrough_reaches_verb(capsys):
    # `semver validate <v>` reaches semver.main's own dispatch, which prints
    # valid/invalid and exits 0 — proving the single-leaf passthrough.
    rc = cli_entry.main(["semver", "validate", "1.2.3"])
    out = capsys.readouterr().out
    assert rc == 0
    assert out.strip() == "valid"


def test_semver_help_passthrough(capsys):
    rc = cli_entry.main(["semver", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "semver" in text


def test_changelog_add_dispatches_to_add_main(capsys):
    # `changelog add --help` forwards `--help` verbatim to changelog.add_main,
    # which has no help flag and treats it as a (bad) slug — its slug error
    # naming `--help` proves the passthrough reached the verb, not click.
    rc = cli_entry.main(["changelog", "add", "--help"])
    err = capsys.readouterr().err
    assert rc == 2
    assert "slug" in err and "--help" in err


def test_changelog_bare_runs_orchestrator(capsys):
    # Bare `changelog` (no subcommand) delegates to orchestrator_main, which
    # prints the orchestrator usage on stderr and exits 2 — matching bin/changelog.
    rc = cli_entry.main(["changelog"])
    err = capsys.readouterr().err
    assert rc == 2
    assert "changelog" in err.lower()


def test_changelog_help_lists_subcommands(capsys):
    rc = cli_entry.main(["changelog", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "add" in out and "cut" in out and "render" in out


def test_sync_drift_check_dispatches_to_drift_verb(capsys):
    rc = cli_entry.main(["sync", "drift-check", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    # release_drift_check.main prints its own drift help/usage.
    assert "drift" in text.lower()


def test_issue_file_dispatches_to_gh_release_issue_help(capsys):
    rc = cli_entry.main(["issue", "file", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "gh-release-issue" in text


# --- the attach() shape includes the newly-filled per-project surface ------


def test_attach_includes_filled_surface():
    fresh = click.Group(name="x")
    toplevel.attach(fresh)
    assert {"detect-kind", "audit", "semver", "changelog", "sync", "issue"} <= set(fresh.commands)
