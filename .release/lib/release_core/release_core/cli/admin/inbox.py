"""``release-core admin inbox`` — consumer-feedback inbox.

Shape (← old name): a group with BOTH a bare invocation AND a subcommand::

  admin inbox                 ← release-inbox          (the bare triage view)
  admin inbox notify-source   ← release-notify-source  (close-the-loop notice)

Bare ``admin inbox`` runs the triage view. A plain ``invoke_without_command``
group can't BOTH dispatch a named subcommand AND forward an option-leading bare
invocation (``admin inbox --json``) — click insists on resolving the leading
token as a subcommand. So the group is a small :class:`_InboxGroup` whose
``parse_args`` checks whether the first argv token (``argv[0]``) names a real
subcommand: if so it dispatches normally; otherwise it forwards the whole argv
verbatim to :func:`release_core.verbs.release_inbox.main` (its own ``--help``
and usage errors included). ``notify-source`` is a plain ``wrap_verb`` leaf.
"""

from __future__ import annotations

import click

from ...verbs import release_inbox, release_notify_source
from .._helpers import wrap_verb


class _InboxGroup(click.Group):
    """A group whose bare form is itself a passthrough verb.

    A plain ``invoke_without_command`` group can't both (a) dispatch a named
    subcommand and (b) forward an option-leading bare invocation (``admin inbox
    --json``) to a verb — click insists on resolving the leading token as a
    subcommand and chokes on ``--json``. So we intercept in ``parse_args``: if
    the FIRST argv token names a real subcommand, defer to normal click
    dispatch; otherwise treat the whole argv as the bare-form payload (stashed
    on the context) and forward it verbatim to release-inbox in the callback
    (its own ``--help`` included).

    The check is the first token specifically — not the first non-flag token
    anywhere — because this group defines no options of its own, so a subcommand
    can only ever be ``argv[0]``. Scanning further would mis-dispatch when a
    release-inbox option *value* happens to equal a subcommand name (e.g.
    ``admin inbox --label notify-source``).
    """

    def parse_args(self, ctx: click.Context, args: list[str]) -> list[str]:
        if args and args[0] in self.commands:
            return super().parse_args(ctx, args)
        # Bare form: stash the whole argv for the callback to forward to
        # release-inbox untouched, and consume it so click parses nothing.
        ctx.meta["inbox_argv"] = list(args)
        return []


@click.group(
    name="inbox",
    cls=_InboxGroup,
    short_help="Consumer-feedback inbox: bare = triage view; notify-source.",
    invoke_without_command=True,
)
@click.pass_context
def group(ctx: click.Context) -> None:
    """The #348 consumer-feedback inbox.

    Bare ``admin inbox`` runs the read-only triage view over the
    ``consumer-filed`` issues on this repo (← release-inbox); all of its flags
    (``--json`` / ``--label`` / ``--repo``) pass straight through. Use the
    ``notify-source`` subcommand to close the loop on a filed issue.
    """
    if ctx.invoked_subcommand is None:
        raise SystemExit(release_inbox.main(ctx.meta.get("inbox_argv", [])))


group.add_command(
    wrap_verb(
        release_notify_source.main,
        name="notify-source",
        short_help="Notify source PRs that an upstream fix shipped.",
    )
)
