"""release_gh — a reviewer-agnostic GitHub PR state engine.

The stable core of the development-workflow layer: a read-only model of where
a PR stands (which reviewers are pending/done, which threads are open, whether
it is mergeable), with reviewer-specific mechanics isolated in swappable
adapters so the core never names a reviewer.

Boundary discipline: every GitHub call goes through `ghapi` (shell out to
`gh`); everything else is pure transformation over recorded data, so it unit-
tests against captured JSON with no network. stdlib only — no third-party
runtime deps. See docs/proposals/dev-workflow-state-engine.lex.
"""

__version__ = "0.0.1"
