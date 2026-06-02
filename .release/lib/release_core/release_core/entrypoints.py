"""Console-script entry points. Each wrapper reads ``sys.argv`` and delegates to
a verb's ``main(argv) -> int`` (``changelog`` keeps ``orchestrator_main``; its
add/cut/render shims map to the dedicated ``add_main``/``cut_main``/``render_main``
functions). Wrappers exist so the verb modules stay free of console-script
plumbing — they keep their pure ``main(argv: list[str]) -> int`` signature.

The ``[project.scripts]`` table in ``pyproject.toml`` maps each on-PATH command
name (hyphenated, matching today's ``bin/`` shims) to one wrapper here. The set
of wrappers == exactly the ``bin/`` shims that dispatch to ``release_core``.
Bash tools (``fetch-deps``/``fetch-artifact``/``gh-*``/``clone-*``) and
``release_gh``-backed tools (``gh-task-status``) are intentionally absent.
"""

from __future__ import annotations

import sys

from release_core.verbs import (
    apply_ruleset,
    audit_portfolio,
    audit_repo,
    audit_smoke_test,
    changelog,
    detect_kind,
    done_check,
    enable_dependabot_security,
    gh_release_issue,
    install_release_secrets,
    install_release_token,
    list_repo_pr,
    list_repo_scripts,
    managed_repos,
    release_advance_major,
    release_beta_list,
    release_cut,
    release_drift_check,
    release_inbox,
    release_lex,
    release_notify_source,
    release_sync,
    release_verify_fleet,
    semver,
    sweep_github_policy,
)


def apply_ruleset_main() -> None:
    raise SystemExit(apply_ruleset.main(sys.argv[1:]))


def audit_portfolio_main() -> None:
    raise SystemExit(audit_portfolio.main(sys.argv[1:]))


def audit_repo_main() -> None:
    raise SystemExit(audit_repo.main(sys.argv[1:]))


def audit_smoke_test_main() -> None:
    raise SystemExit(audit_smoke_test.main(sys.argv[1:]))


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


def done_check_main() -> None:
    raise SystemExit(done_check.main(sys.argv[1:]))


def enable_dependabot_security_main() -> None:
    raise SystemExit(enable_dependabot_security.main(sys.argv[1:]))


def gh_release_issue_main() -> None:
    raise SystemExit(gh_release_issue.main(sys.argv[1:]))


def install_release_secrets_main() -> None:
    raise SystemExit(install_release_secrets.main(sys.argv[1:]))


def install_release_token_main() -> None:
    raise SystemExit(install_release_token.main(sys.argv[1:]))


def list_repo_pr_main() -> None:
    raise SystemExit(list_repo_pr.main(sys.argv[1:]))


def list_repo_scripts_main() -> None:
    raise SystemExit(list_repo_scripts.main(sys.argv[1:]))


def managed_repos_main() -> None:
    raise SystemExit(managed_repos.main(sys.argv[1:]))


def release_advance_major_main() -> None:
    raise SystemExit(release_advance_major.main(sys.argv[1:]))


def release_beta_list_main() -> None:
    raise SystemExit(release_beta_list.main(sys.argv[1:]))


def release_cut_main() -> None:
    raise SystemExit(release_cut.main(sys.argv[1:]))


def release_drift_check_main() -> None:
    raise SystemExit(release_drift_check.main(sys.argv[1:]))


def release_inbox_main() -> None:
    raise SystemExit(release_inbox.main(sys.argv[1:]))


def release_lex_main() -> None:
    raise SystemExit(release_lex.main(sys.argv[1:]))


def release_notify_source_main() -> None:
    raise SystemExit(release_notify_source.main(sys.argv[1:]))


def release_sync_main() -> None:
    raise SystemExit(release_sync.main(sys.argv[1:]))


def release_verify_fleet_main() -> None:
    raise SystemExit(release_verify_fleet.main(sys.argv[1:]))


def semver_main() -> None:
    raise SystemExit(semver.main(sys.argv[1:]))


def sweep_github_policy_main() -> None:
    raise SystemExit(sweep_github_policy.main(sys.argv[1:]))
