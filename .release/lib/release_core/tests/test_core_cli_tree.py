"""The assembled ``release-core`` group tree: shape + help one-liners + the
exemplar dispatch (release#460).

Pins what the parallel agents build on: the full group skeleton is registered
(so ``--help`` shows the whole shape), the exemplar ``pr`` group is fully wired
(nested ``copilot`` subgroup, ``wrap_script`` leaves, ``pr status``), the
top-level verb-wrap exemplars (``cut``, ``status``) dispatch, every leaf carries
a one-line short_help, and unimplemented leaves are registered stubs.
"""

from __future__ import annotations

import click
from release_core import cli_entry
from release_core.cli import _helpers, admin, ci, pr, toplevel


def _root() -> click.Group:
    """The assembled root group (cli_entry.root) — for tree introspection."""
    return cli_entry.root


# --- the full skeleton is registered (the "map") --------------------------


def test_toplevel_groups_registered():
    root = _root()
    names = set(root.commands)
    assert {"pr", "ci", "admin"} <= names
    # per-project flat verbs + folded-in init/selfcheck
    assert {"init", "selfcheck", "cut", "status"} <= names
    # per-project stub groups/commands
    assert {"changelog", "semver", "sync", "detect-kind", "audit", "issue"} <= names


def test_pr_group_is_fully_wired():
    grp = pr.group
    assert set(grp.commands) >= {"copilot", "checks-wait", "resolve-thread", "status"}
    copilot = grp.commands["copilot"]
    assert isinstance(copilot, click.Group)
    assert set(copilot.commands) == {"on", "off", "wait", "review"}


def test_admin_skeleton_registered():
    grp = admin.group
    assert set(grp.commands) >= {
        "repos",
        "release",
        "policy",
        "secrets",
        "inbox",
        "smoke-test",
    }
    assert set(admin.repos.group.commands) >= {
        "list",
        "prs",
        "scripts",
        "audit",
        "verify",
    }
    assert set(admin.release_cmds.group.commands) >= {"advance-major", "betas", "lex"}
    assert set(admin.policy.group.commands) >= {"ruleset", "sweep", "dependabot"}
    assert set(admin.secrets.group.commands) >= {"install", "token"}
    assert set(admin.inbox.group.commands) >= {"notify-source"}


def test_ci_group_registered_as_stub():
    # ci is a registered (empty) group for a parallel agent to fill.
    assert isinstance(ci.group, click.Group)
    assert ci.group.name == "ci"


# --- every leaf has a one-line short_help (discoverability requirement) ----


def _walk(grp: click.Group):
    for cmd in grp.commands.values():
        if isinstance(cmd, click.Group):
            yield from _walk(cmd)
        else:
            yield cmd


def test_every_leaf_has_a_short_help():
    root = _root()
    missing = [c.name for c in _walk(root) if not (c.short_help or "").strip()]
    assert missing == [], f"leaves missing short_help: {missing}"


# --- exemplar dispatch ----------------------------------------------------


def test_cut_dispatches_to_release_cut_help(capsys):
    # cut --help forwards to the release_cut verb's own help (exit 0). The verb
    # prints its usage to stderr; the passthrough preserves the stream faithfully.
    rc = cli_entry.main(["cut", "--help"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "release-cut" in (captured.out + captured.err)


def test_status_dispatches_to_done_check_help(capsys):
    rc = cli_entry.main(["status", "--help"])
    captured = capsys.readouterr()
    text = captured.out + captured.err
    assert rc == 0
    # done_check's help mentions its pilot-running / done-check contract.
    assert "done-check" in text or "done_check" in text or "pilot" in text.lower()


def test_pr_status_dispatches_to_task_status_help(capsys):
    rc = cli_entry.main(["pr", "status", "--help"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "gh-task-status" in (captured.out + captured.err)


# --- stub behavior --------------------------------------------------------


def test_invoking_a_stub_leaf_exits_69(capsys):
    # Exercise the stub-leaf factory itself on a THROWAWAY group, so the test no
    # longer depends on any real (now-implemented) command staying a stub.
    root = click.Group(name="x")
    root.add_command(toplevel._stub_command("synth", "A synthetic stub leaf."))
    try:
        root.main(args=["synth"], prog_name="x", standalone_mode=False)
        rc = 0
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    err = capsys.readouterr().err
    assert rc == 69
    assert "stub" in err.lower()


def test_stub_leaf_with_extra_args_still_exits_69(capsys):
    # Any flags/args land on the stub-exit path (69), not click's usage error.
    root = click.Group(name="x")
    root.add_command(toplevel._stub_command("synth", "A synthetic stub leaf."))
    try:
        root.main(
            args=["synth", "--json", "extra", "-x"],
            prog_name="x",
            standalone_mode=False,
        )
        rc = 0
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    err = capsys.readouterr().err
    assert rc == 69
    assert "stub" in err.lower()


def test_stub_leaf_help_is_still_reachable(capsys):
    # --help remains discoverable on a stub (shows the short_help / usage).
    # Exercised on the SYNTHETIC stub leaf so the stub-factory coverage stays
    # self-contained as real commands stop being stubs over time.
    root = click.Group(name="x")
    root.add_command(toplevel._stub_command("synth", "A synthetic stub leaf."))
    try:
        root.main(args=["synth", "--help"], prog_name="x", standalone_mode=False)
        rc = 0
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    out = capsys.readouterr().out
    assert rc == 0
    assert "synth" in out or "Usage:" in out


def test_bare_empty_stub_group_exits_69_not_silent_0(capsys):
    # An EMPTY stub group invoked bare prints help + a stub note and exits 69 —
    # never a silent exit 0 (Copilot review #463). Every real group is now wired
    # (changelog/ci by #465; admin policy/secrets/inbox + smoke-test by #466), so
    # this exercises the stub_group factory on a SYNTHETIC group, keeping the
    # factory's coverage self-contained as real groups stop being stubs.
    root = click.Group(name="x")
    root.add_command(_helpers.stub_group("synthgrp", short_help="A synthetic stub group."))
    try:
        root.main(args=["synthgrp"], prog_name="x", standalone_mode=False)
        rc = 0
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    captured = capsys.readouterr()
    assert rc == 69
    assert "stub" in captured.err.lower()
    # help is still shown (the map), on stdout.
    assert "Usage:" in captured.out


def test_bare_stub_group_help_flag_exits_0(capsys):
    # `<stub group> --help` is discovery, exit 0 (not the bare stub-exit path).
    rc = cli_entry.main(["changelog", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out


def test_nonempty_stub_group_bare_is_missing_command_not_silent(capsys):
    # A group WITH subcommands (stub leaves) invoked bare is click's
    # "Missing command" usage error (exit 2) — still never a silent 0.
    rc = cli_entry.main(["admin", "repos"])
    assert rc == 2


def test_toplevel_attach_is_idempotent_shape():
    # attach() is what cli_entry calls; calling it on a fresh group yields the
    # same per-project command set (proves no hidden global state).
    fresh = click.Group(name="x")
    toplevel.attach(fresh)
    assert {"init", "selfcheck", "cut", "status"} <= set(fresh.commands)
