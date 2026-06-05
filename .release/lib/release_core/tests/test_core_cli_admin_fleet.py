"""``release-core admin repos`` + ``admin release`` — the filled fleet groups.

Pins the two groups this agent wired (release#460 / epic #461): both are real
``click.Group``s (not stub groups), every intended leaf is registered with a
non-empty ``short_help``, and each leaf is a faithful ``wrap_verb`` passthrough
that dispatches ``--help`` to its underlying verb.
"""

from __future__ import annotations

import click
from release_core import cli_entry
from release_core.cli import admin
from release_core.cli.admin import release_cmds, repos

REPOS_LEAVES = {"list", "prs", "scripts", "audit", "verify"}
RELEASE_LEAVES = {"advance-major", "betas", "lex"}


# --- the groups are real and attached -------------------------------------


def test_repos_is_a_real_group_with_all_leaves():
    assert isinstance(repos.group, click.Group)
    assert repos.group.name == "repos"
    assert set(repos.group.commands) == REPOS_LEAVES


def test_release_is_a_real_group_with_all_leaves():
    assert isinstance(release_cmds.group, click.Group)
    assert release_cmds.group.name == "release"
    assert set(release_cmds.group.commands) == RELEASE_LEAVES


def test_groups_are_attached_under_admin():
    assert admin.group.commands["repos"] is repos.group
    assert admin.group.commands["release"] is release_cmds.group


# --- discoverability: every leaf carries a one-line short_help -------------


def test_every_leaf_has_short_help():
    for grp in (repos.group, release_cmds.group):
        for cmd in grp.commands.values():
            assert (cmd.short_help or "").strip(), f"{grp.name} {cmd.name}"


# --- bare invocation is click's Missing-command (exit 2), never silent 0 ----


def test_bare_groups_are_missing_command_not_silent():
    for args in (["admin", "repos"], ["admin", "release"]):
        assert cli_entry.main(args) == 2, args


# --- leaves are passthroughs: --help reaches the underlying verb -----------


def test_repos_list_help_reaches_managed_repos(capsys):
    rc = cli_entry.main(["admin", "repos", "list", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "managed-repos" in text or "managed_repos" in text


def test_repos_verify_help_reaches_verify_fleet(capsys):
    rc = cli_entry.main(["admin", "repos", "verify", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "verify" in text.lower()


def test_release_advance_major_help_reaches_verb(capsys):
    rc = cli_entry.main(["admin", "release", "advance-major", "--help"])
    text = "".join(capsys.readouterr())
    assert rc == 0
    assert "advance" in text.lower() or "major" in text.lower()


def test_no_leaf_is_a_stub_anymore(capsys):
    # A stub leaf exits 69; a wired wrap_verb leaf must not. Probe a couple via
    # --help (exit 0, never the 69 stub path).
    for args in (
        ["admin", "repos", "prs", "--help"],
        ["admin", "release", "betas", "--help"],
    ):
        rc = cli_entry.main(args)
        captured = capsys.readouterr()
        assert rc == 0, f"{args} exited {rc}\n{captured.out}\n{captured.err}"
