"""release_core.prstate — a reviewer-agnostic GitHub PR state engine.

The stable core of the development-workflow layer: a read-only model of where
a PR stands (which reviewers are pending/done, which threads are open, whether
it is mergeable), with reviewer-specific mechanics isolated in swappable
adapters so the core never names a reviewer.

Folded into release_core (was the standalone, sync-distributed `release_gh`
package; release#459) so it ships via the one pip wheel. Its `ghapi` boundary
is kept distinct from `release_core.gh`: this subpackage is stdlib-only (it runs
the same in CI / Claude Cloud / local), whereas `release_core.gh` is the
verb-layer helper. Both shell out to `gh`; the duplication is intentional and
load-bearing for the stdlib-only guarantee here.

Boundary discipline: every GitHub call goes through `ghapi` (shell out to
`gh`); everything else is pure transformation over recorded data, so it unit-
tests against captured JSON with no network. stdlib only — no third-party
runtime deps.
"""

__version__ = "0.0.1"
