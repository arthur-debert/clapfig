"""Console-script entry points. Each wrapper reads ``sys.argv`` and delegates to
a verb's ``main(argv) -> int`` (``changelog`` keeps ``orchestrator_main``; its
add/cut/render shims map to the dedicated ``add_main``/``cut_main``/``render_main``
functions). Wrappers exist so the verb modules stay free of console-script
plumbing — they keep their pure ``main(argv: list[str]) -> int`` signature.

The ``[project.scripts]`` table in ``pyproject.toml`` maps each on-PATH command
name to one wrapper here. After the CLI cutover (#468, epic #461) this is the
SHORT LIST of CONSUMER-FACING aliases only — the maintainer/fleet verbs are
reachable exclusively through the ``release-core <group> <command>`` tree (see
``release_core.cli``), not as flat console-scripts. Bash tools
(``fetch-deps``/``fetch-artifact``/``gh-*``/``clone-*``) are intentionally
absent. ``gh-task-status`` is here too: the PR state engine was folded in
(``release_core.prstate``; release#459), so it ships as a console script from
the one wheel instead of by sync.
"""

from __future__ import annotations

import sys

from release_core.prstate.cli import task_status
from release_core.verbs import (
    changelog,
    detect_kind,
    gh_release_issue,
    release_drift_check,
    release_sync,
    semver,
)


def changelog_main() -> None:
    raise SystemExit(changelog.orchestrator_main(sys.argv[1:]))


def changelog_add_main() -> None:
    raise SystemExit(changelog.add_main(sys.argv[1:]))


def changelog_cut_main() -> None:
    raise SystemExit(changelog.cut_main(sys.argv[1:]))


def changelog_render_main() -> None:
    raise SystemExit(changelog.render_main(sys.argv[1:]))


def detect_kind_main() -> None:
    raise SystemExit(detect_kind.main(sys.argv[1:]))


def gh_release_issue_main() -> None:
    raise SystemExit(gh_release_issue.main(sys.argv[1:]))


def gh_task_status_main() -> None:
    raise SystemExit(task_status.main(sys.argv[1:]))


def release_drift_check_main() -> None:
    raise SystemExit(release_drift_check.main(sys.argv[1:]))


def release_sync_main() -> None:
    raise SystemExit(release_sync.main(sys.argv[1:]))


def semver_main() -> None:
    raise SystemExit(semver.main(sys.argv[1:]))
