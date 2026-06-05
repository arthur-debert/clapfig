"""``release-core admin`` onboarding groups — policy / secrets / inbox + the
flat ``smoke-test`` leaf (release#460, epic #461).

Pins that the three filled groups (``admin policy``, ``admin secrets``,
``admin inbox``) and the ``admin smoke-test`` flat leaf are registered, carry
one-line ``short_help``, and dispatch through to their backing verbs. The
interesting case is ``admin inbox``: its BARE form runs the release-inbox
triage view, while ``admin inbox notify-source`` dispatches the subcommand —
both must work off the same group.
"""

from __future__ import annotations

import click
from release_core import cli_entry, gh
from release_core.cli import admin

# --- registration + short_help -------------------------------------------


def test_policy_group_is_wired():
    grp = admin.policy.group
    assert set(grp.commands) >= {"ruleset", "sweep", "dependabot"}
    for cmd in grp.commands.values():
        assert isinstance(cmd, click.Command)
        assert (cmd.short_help or "").strip(), cmd.name


def test_secrets_group_is_wired():
    grp = admin.secrets.group
    assert set(grp.commands) >= {"install", "token"}
    for cmd in grp.commands.values():
        assert (cmd.short_help or "").strip(), cmd.name


def test_inbox_group_is_wired():
    grp = admin.inbox.group
    assert isinstance(grp, click.Group)
    assert "notify-source" in grp.commands
    assert (grp.commands["notify-source"].short_help or "").strip()


def test_smoke_test_leaf_is_wired():
    grp = admin.group
    assert "smoke-test" in grp.commands
    leaf = grp.commands["smoke-test"]
    assert not isinstance(leaf, click.Group)
    assert (leaf.short_help or "").strip()


# --- dispatch (leaf --help forwards to the backing verb's own help) --------


def test_policy_ruleset_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "policy", "ruleset", "--help"])
    captured = capsys.readouterr()
    out = captured.out + captured.err
    assert rc == 0
    assert "apply-ruleset" in out


def test_policy_sweep_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "policy", "sweep", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "sweep" in out.lower()


def test_policy_dependabot_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "policy", "dependabot", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "dependabot" in out.lower()


def test_secrets_install_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "secrets", "install", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "install-release-secrets" in out


def test_secrets_token_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "secrets", "token", "--help"])
    captured = capsys.readouterr()
    out = (captured.out + captured.err).lower()
    assert rc == 0
    assert "token" in out


def test_smoke_test_help_is_verb_help(capsys):
    rc = cli_entry.main(["admin", "smoke-test", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "smoke-test" in out.lower()


# --- inbox: bare form runs the triage view, subcommand dispatches ---------


def test_inbox_bare_help_is_release_inbox_help(capsys):
    # Bare `admin inbox --help` forwards to release-inbox's own help, NOT a
    # click group help — the bare form IS the triage verb.
    rc = cli_entry.main(["admin", "inbox", "--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "release-inbox" in out


def test_inbox_bare_json_flag_reaches_release_inbox(capsys, monkeypatch):
    # `--json` is a release-inbox flag; it must pass through untouched and not
    # be mistaken by click for a subcommand. Mock at the data layer (the verb's
    # contract: mock gh.issue_list, no real network).
    monkeypatch.setattr(gh, "issue_list", lambda *a, **k: [])
    rc = cli_entry.main(["admin", "inbox", "--json"])
    out = capsys.readouterr().out
    assert rc == 0
    # JSON output (release-inbox emits a JSON array, even when empty).
    assert out.lstrip().startswith("[")


def test_inbox_bare_unknown_flag_hits_release_inbox_usage(capsys):
    # An unknown flag is release-inbox's own usage error (64), proving the bare
    # form forwards to the verb rather than click parsing it.
    rc = cli_entry.main(["admin", "inbox", "--no-such-flag"])
    err = capsys.readouterr().err
    assert rc == 64
    assert "release-inbox" in err


def test_inbox_notify_source_subcommand_dispatches(capsys):
    # The named subcommand still dispatches to release-notify-source's help.
    rc = cli_entry.main(["admin", "inbox", "notify-source", "--help"])
    captured = capsys.readouterr()
    out = captured.out + captured.err
    assert rc == 0
    assert "release-notify-source" in out


def test_inbox_option_value_matching_subcommand_is_not_dispatched(monkeypatch):
    # A release-inbox option VALUE that happens to equal a subcommand name
    # (e.g. `--label notify-source`) must still be forwarded to the bare verb,
    # NOT mis-dispatched to the `notify-source` subcommand. Only argv[0] selects
    # a subcommand for this group (it defines no options of its own).
    seen = {}

    def fake_issue_list(repo, **kwargs):
        seen["label"] = kwargs.get("label")
        return []

    monkeypatch.setattr(gh, "issue_list", fake_issue_list)
    rc = cli_entry.main(["admin", "inbox", "--label", "notify-source"])
    assert rc == 0
    # release-inbox received the value as its --label, proving it forwarded.
    assert seen["label"] == "notify-source"
