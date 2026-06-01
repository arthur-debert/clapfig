"""release_core — shared stdlib-only primitives for release's Python verbs.

The migration substrate (docs/proposals/shell-to-python.md): one place for the
subprocess/gh/git boundary, the CLI arg harness, YAML I/O, semver math, and
Kind/manifest detection that the per-verb shims (the proven gh-task-status
pattern) build on. stdlib only — no third-party runtime deps — so it rides the
zero-install sys.path shim into every consumer's .release/lib/.

Curated re-exports: the names a verb module reaches for most.
"""

from __future__ import annotations

from . import cli, gh, manifest, proc, version, yamlio
from .cli import EXIT_OK, EXIT_USAGE, Opt, parse
from .gh import GhError
from .proc import ProcError, out, run

__version__ = "0.0.1"

__all__ = [
    "EXIT_OK",
    "EXIT_USAGE",
    "GhError",
    "Opt",
    "ProcError",
    "cli",
    "gh",
    "manifest",
    "out",
    "parse",
    "proc",
    "run",
    "version",
    "yamlio",
]
