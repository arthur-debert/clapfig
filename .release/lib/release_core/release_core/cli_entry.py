"""cli_entry — the top-level ``release-core`` CLI (pip-bootstrap PoC §2).

`release-core` is the package's own command (entry `release_core.cli_entry:main`,
wired in pyproject `[project.scripts]` by PR-B). Unlike the per-verb console
scripts — each a thin wrapper around one verb's ``main(argv) -> int`` — this is a
subcommand dispatcher: ``release-core <subcommand> [args...]``.

PoC subcommands:
  release-core init [--force] [--dry-run]   materialize per-repo committed config
  release-core --help / release-core        print usage, exit 0

The subcommand's own args are forwarded verbatim to its ``main(argv)`` so
`release-core init --help` reaches the init verb's own --help, byte-identical to
invoking the verb directly. Exit code is the subcommand's; an unknown subcommand
is a usage error (exit 64), matching the cli harness convention.
"""

from __future__ import annotations

import sys

from .cli import EXIT_OK, EXIT_USAGE
from .verbs import init as init_verb

# subcommand name -> its main(argv) -> int
_SUBCOMMANDS = {
    "init": init_verb.main,
}

USAGE = """\
release-core — the release tooling CLI.

Usage:
  release-core <subcommand> [args...]
  release-core --help

Subcommands:
  init    Materialize the per-repo committed config (lefthook.yml + lint
          configs) into the current repo. Idempotent, create-if-absent.
          See `release-core init --help`.
"""


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)

    if not args or args[0] in ("-h", "--help"):
        print(USAGE.rstrip("\n"))
        return EXIT_OK

    sub, rest = args[0], args[1:]
    handler = _SUBCOMMANDS.get(sub)
    if handler is None:
        print(f"release-core: unknown subcommand {sub!r}", file=sys.stderr)
        print(USAGE.rstrip("\n"), file=sys.stderr)
        return EXIT_USAGE
    return handler(rest)


if __name__ == "__main__":
    raise SystemExit(main())
