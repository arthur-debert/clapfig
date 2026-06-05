"""Per-project top-level commands of ``release-core`` (the flat verbs a single
repo's agent/human uses), assembled onto the root by :func:`attach`.

Two are EXEMPLARS of the flat ``wrap_verb`` pattern; the rest are now
IMPLEMENTED (release#460):

  cut       ← release_cut.main      (cut a release for this repo)
  status    ← done_check.main       (this repo's pilot-running gate)

``init`` and ``selfcheck`` are folded in here too (verb-wraps), replacing the
hand-rolled dispatcher entries that previously lived in ``cli_entry``.

The per-project surface (← old name):

  cut <version|bump>            ← release-cut            [exemplar]
  status                        ← done-check             [exemplar]
  changelog [add|cut|render]    ← changelog*             [group, bare=orchestrator]
  semver  validate|get          ← semver                 [flat: semver self-dispatches]
  sync [run|drift-check]        ← release-sync / release-drift-check  [group, bare=sync]
  detect-kind                   ← detect-kind            [flat]
  audit                         ← audit-repo             [flat]
  issue file <component> <msg>  ← gh-release-issue       [group]

``_stub_command`` / ``stub_group`` scaffolding stays available for the still-stub
groups owned by other group modules.
"""

from __future__ import annotations

import click

from ..verbs import (
    audit_repo,
    changelog,
    detect_kind,
    done_check,
    gh_release_issue,
    init,
    release_cut,
    release_drift_check,
    release_sync,
    selfcheck,
    semver,
)
from ._helpers import STUB_EXIT, wrap_verb


def attach(root: click.Group) -> None:
    """Attach every per-project top-level command/group to ``root``.

    Called by ``cli_entry`` exactly once.
    """

    # --- folded-in dispatcher commands (were in the hand-rolled cli_entry) ---
    root.add_command(
        wrap_verb(
            init.main,
            name="init",
            short_help="Materialize this repo's committed release config.",
        )
    )
    root.add_command(
        wrap_verb(
            selfcheck.main,
            name="selfcheck",
            short_help="Verify release-core's runtime deps are importable.",
        )
    )

    # --- EXEMPLARS: flat wrap_verb commands -------------------------------
    root.add_command(
        wrap_verb(
            release_cut.main,
            name="cut",
            short_help="Cut a release for this repo (any Kind).",
        )
    )
    root.add_command(
        wrap_verb(
            done_check.main,
            name="status",
            short_help="This repo's pilot-running gate (done-check).",
        )
    )

    # --- flat per-project commands ----------------------------------------
    root.add_command(
        wrap_verb(
            detect_kind.main,
            name="detect-kind",
            short_help="Detect this repo's release Kind.",
        )
    )
    root.add_command(
        wrap_verb(
            audit_repo.main,
            name="audit",
            short_help="Audit THIS repo's release posture.",
        )
    )
    # semver self-dispatches its own validate/get positional verbs, so it stays
    # a single passthrough leaf (NOT a group) — argv reaches semver.main intact.
    root.add_command(
        wrap_verb(
            semver.main,
            name="semver",
            short_help="Validate a version or extract a semver part (validate/get).",
        )
    )

    # --- small per-project groups -----------------------------------------
    root.add_command(_changelog_group())
    root.add_command(_sync_group())
    root.add_command(_issue_group())


# --------------------------------------------------------------------------
# Per-project groups.
# --------------------------------------------------------------------------


def _changelog_group() -> click.Group:
    """``changelog`` — bare runs the orchestrator; add/cut/render are leaves.

    Bare ``changelog`` (no subcommand) delegates to ``orchestrator_main`` so the
    behavior matches today's ``bin/changelog`` (it prints the orchestrator usage
    on stderr and exits 2). The add/cut/render subcommands are registered as
    discoverable ``wrap_verb`` leaves over the dedicated ``*_main`` functions, so
    ``changelog add ...`` is byte-identical to ``changelog add ...`` via the shim.
    """

    @click.group(
        name="changelog",
        short_help="Manage this repo's changelog (add/cut/render).",
        invoke_without_command=True,
    )
    @click.pass_context
    def _grp(ctx: click.Context) -> None:
        """Changelog management.

        Bare ``changelog`` runs the orchestrator (prints usage); use the
        ``add`` / ``cut`` / ``render`` subcommands for the individual steps.
        """
        if ctx.invoked_subcommand is None:
            raise SystemExit(changelog.orchestrator_main([]))

    _grp.add_command(
        wrap_verb(
            changelog.add_main,
            name="add",
            short_help="Add an unreleased changelog fragment.",
        )
    )
    _grp.add_command(
        wrap_verb(
            changelog.cut_main,
            name="cut",
            short_help="Cut unreleased fragments into a version's changelog file.",
        )
    )
    _grp.add_command(
        wrap_verb(
            changelog.render_main,
            name="render",
            short_help="Regenerate CHANGELOG.md from the version files.",
        )
    )
    return _grp


def _sync_group() -> click.Group:
    """``sync`` — bare (and ``run``) materialize the synced tree; drift-check gates.

    Bare ``sync`` delegates to ``release_sync.main`` so it stays byte-identical to
    ``bin/release-sync``; ``sync run`` is the explicit alias. ``sync drift-check``
    wraps ``release_drift_check.main`` (the consumer-side drift gate).
    """

    @click.group(
        name="sync",
        short_help="Materialize / drift-check the synced .release/ tree.",
        invoke_without_command=True,
        # ignore_unknown_options lets bare `sync --some-release-sync-flag` collect
        # the flag into ctx.args and forward it to release_sync.main, while click
        # still intercepts `--help` (default help_option_names) so `sync --help`
        # shows the group map. Power users pass flags via the explicit `sync run`.
        context_settings={
            "ignore_unknown_options": True,
            "allow_extra_args": True,
        },
    )
    @click.pass_context
    def _grp(ctx: click.Context) -> None:
        """Sync helpers.

        Bare ``sync`` (or ``sync run``) rebuilds the consumer's ``.release/``
        build dir + symlinks (release-sync); ``sync drift-check`` rebuilds against
        the recorded source revision and fails on real drift.
        """
        if ctx.invoked_subcommand is None:
            raise SystemExit(release_sync.main(list(ctx.args)))

    _grp.add_command(
        wrap_verb(
            release_sync.main,
            name="run",
            short_help="Materialize the synced .release/ build dir + symlinks.",
        )
    )
    _grp.add_command(
        wrap_verb(
            release_drift_check.main,
            name="drift-check",
            short_help="Fail if the synced .release/ tree has drifted from source.",
        )
    )
    return _grp


def _issue_group() -> click.Group:
    """``issue`` — escalate infra friction to arthur-debert/release."""

    @click.group(
        name="issue",
        short_help="Escalate infra friction to arthur-debert/release.",
    )
    def _grp() -> None:
        """Issue helpers.

        Escalate infrastructure friction (workflow failures, broken policy
        templates, helper-script bugs) from this repo up to the canonical
        arthur-debert/release tracker.
        """

    _grp.add_command(
        wrap_verb(
            gh_release_issue.main,
            name="file",
            short_help="File (or comment on) a release-issue from this repo.",
        )
    )
    return _grp


# --------------------------------------------------------------------------
# Stub scaffolding. Kept available for the still-stub groups in sibling
# modules. A stub leaf or a bare-invoked stub group exits STUB_EXIT (69,
# EX_UNAVAILABLE) with a clear "not yet wired" message, so it can never be
# mistaken for working.
# --------------------------------------------------------------------------


class _StubCommand(click.Command):
    """A registered-but-unimplemented LEAF: shows in ``--help`` like any other
    command, but exits ``STUB_EXIT`` with a clear message if actually invoked,
    so it can never be mistaken for working.

    It accepts *any* args/flags (``ignore_unknown_options`` + a catch-all
    ``args`` param) so that even ``release-core <stub> --json foo`` lands on
    the stub-exit path rather than failing with click's usage error (exit 2).
    ``--help`` is still handled (eager), so the stub's help stays discoverable.
    """

    def invoke(self, ctx: click.Context):
        click.echo(
            f"release-core: `{self.name}` is a registered stub (not yet "
            f"wired to its verb — see #460).",
            err=True,
        )
        raise SystemExit(STUB_EXIT)


def _stub_command(name: str, short_help: str) -> click.Command:
    cmd = _StubCommand(
        name=name,
        short_help=short_help,
        callback=lambda args=None: None,
        context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
        params=[click.Argument(["args"], nargs=-1, type=click.UNPROCESSED)],
    )
    return cmd
