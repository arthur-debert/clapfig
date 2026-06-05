"""``release_core.cli`` — two things under one name, by design:

1. The **shared CLI harness** (``Opt`` / :func:`parse` / ``EXIT_OK`` /
   ``EXIT_USAGE``) that the verb modules use for their own ``--help`` and
   uniform usage-error exit codes. This was historically ``cli.py``; it is
   folded in here verbatim so ``from ..cli import EXIT_OK, Opt, parse`` keeps
   working byte-for-byte for every verb that imports it.

2. The **hierarchical click command tree** (``release-core <group> <command>``),
   one module per top-level group, assembled in :mod:`release_core.cli_entry`:

   - :mod:`._helpers`   — the two wrapping patterns (``wrap_verb`` /
                          ``wrap_script``) + the ``run_root`` click→int bridge.
   - :mod:`.toplevel`   — per-project flat commands + small per-project groups
                          (``changelog`` / ``semver`` / ``sync`` / ``issue`` …).
   - :mod:`.pr`         — the ``pr`` group (PR-loop helpers; EXEMPLAR).
   - :mod:`.ci`         — the ``ci`` group (fetch-deps / fetch-artifact; stub).
   - :mod:`.admin`      — the ``admin`` subpackage (fleet/meta-release), itself
                          split into ``repos`` / ``release_cmds`` / ``policy`` /
                          ``secrets`` / ``inbox``.

   See ``docs/dev/release-core-cli-pattern.md`` for the authoring rules.

A verb declares its options declaratively as a list of :class:`Opt`; the harness
handles ``-h``/``--help`` (printing the module docstring — the single source of
help text, no separate ``show_help()``), a uniform usage-error exit code (64),
and ``--json``-style boolean flags. Verb modules call :func:`parse` at the top
of ``main()``.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass

EXIT_OK = 0
EXIT_USAGE = 64


@dataclass
class Opt:
    """A single declared option.

    ``name`` is the long form including dashes ("--repo"). A value-less option
    (``takes_value=False``) is a boolean flag: present → True, absent →
    ``default`` (normally False).
    """

    name: str
    takes_value: bool = False
    default: object = None
    help: str = ""

    @property
    def key(self) -> str:
        """The dict key the parsed value lands under: long name without dashes."""
        return self.name.lstrip("-")


def _usage_error(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)
    raise SystemExit(EXIT_USAGE)


def parse(
    argv: list[str],
    opts: list[Opt],
    *,
    positionals: tuple[int, int] = (0, 0),
    doc: str = "",
) -> tuple[dict, list[str]]:
    """Parse ``argv`` against declared ``opts``.

    Returns ``(values, positionals)`` where ``values`` is keyed by each option's
    long name without dashes, seeded with every option's ``default``.

    ``-h``/``--help`` prints ``doc`` and raises ``SystemExit(0)``. An unknown
    option, a missing value for a value-taking option, or a positional count
    outside the inclusive ``positionals`` (min, max) range prints to stderr and
    raises ``SystemExit(64)``.
    """
    by_name = {opt.name: opt for opt in opts}
    values: dict = {opt.key: opt.default for opt in opts}
    rest: list[str] = []
    saw_separator = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if not saw_separator and arg in ("-h", "--help"):
            print(doc.rstrip("\n") if doc else "(no help available)")
            raise SystemExit(EXIT_OK)
        if not saw_separator and arg == "--":
            # Everything after `--` is a positional, even if dash-led.
            saw_separator = True
            i += 1
            continue
        if not saw_separator and arg.startswith("-") and arg != "-":
            # Support `--opt=value` as well as `--opt value`.
            if "=" in arg:
                name, _, inline = arg.partition("=")
            else:
                name, inline = arg, None
            opt = by_name.get(name)
            if opt is None:
                _usage_error(f"unknown option {name}")
            if opt.takes_value:
                if inline is not None:
                    values[opt.key] = inline
                else:
                    if i + 1 >= len(argv):
                        _usage_error(f"option {name} requires a value")
                    i += 1
                    values[opt.key] = argv[i]
            else:
                if inline is not None:
                    _usage_error(f"option {name} takes no value")
                values[opt.key] = True
            i += 1
            continue
        rest.append(arg)
        i += 1

    lo, hi = positionals
    if not (lo <= len(rest) <= hi):
        want = f"{lo}" if lo == hi else f"{lo}–{hi}"
        _usage_error(f"expected {want} positional argument(s), got {len(rest)}")

    return values, rest
