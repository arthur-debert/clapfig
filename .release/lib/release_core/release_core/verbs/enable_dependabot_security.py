"""enable-dependabot-security — enable Dependabot security updates on onboarded repos.

Enable Dependabot security updates (alerts + automated fixes) on every repo
onboarded to the canonical main-branch-protection ruleset.

Why: per the release/README.md "Dependabot policy", security updates are the
only flow worth automating across the portfolio. They're toggled via the
GitHub API, not the dependabot.yml file — so this script is the canonical
way to apply the policy.

Endpoints used:
  PUT /repos/{repo}/vulnerability-alerts      — enables Dependabot alerts
  PUT /repos/{repo}/automated-security-fixes  — enables auto-PR for security

Usage:
  enable-dependabot-security                              # auto-discover
  enable-dependabot-security --owners arthur-debert,lex-fmt
  enable-dependabot-security --repos arthur-debert/dodot,lex-fmt/lex
  enable-dependabot-security --dry-run

Shell→Python migration (docs/proposals/shell-to-python.md): the rulesets-membership
discovery (gh + jq) and the two enable PUTs (`gh api -X PUT … --silent`) move into
Python. The PUTs go through gh.rest(..., method="PUT") (no body); stdout (discovery
header, per-repo ok/FAIL lines, summary) and exit codes are preserved byte-for-byte.
"""

from __future__ import annotations

import sys

from .. import cli, gh

USAGE = __doc__ or ""

_RULESET_NAME = "main-branch-protection"


def _usage_block() -> str:
    """The bash `--help` block (lines 2..first-blank, `# ` stripped)."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


# --------------------------------------------------------------------------
# Pure helper — fixture-tested.
# --------------------------------------------------------------------------


def has_ruleset(rulesets: object, name: str = _RULESET_NAME) -> bool:
    """True if any ruleset in the payload is named `name`. Mirrors the bash jq+head."""
    return any(isinstance(rs, dict) and rs.get("name") == name for rs in rulesets or [])


# --------------------------------------------------------------------------
# gh boundary.
# --------------------------------------------------------------------------


def _list_owner_repos(owner: str) -> list[str]:
    """`gh repo list <owner> --limit 200 --json nameWithOwner` → list of full names."""
    import json

    result = gh.repo_list(owner, limit=200, json_fields=["nameWithOwner"], check=False)
    if result.returncode != 0 or not result.stdout.strip():
        return []
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        return []
    return [e["nameWithOwner"] for e in data if isinstance(e, dict) and e.get("nameWithOwner")]


def _is_onboarded(repo: str) -> bool:
    """True if `repo` carries the main-branch-protection ruleset (the bash discovery filter)."""
    try:
        rulesets = gh.rest(f"repos/{repo}/rulesets")
    except gh.GhError:
        return False
    return has_ruleset(rulesets)


def discover_repos(owners: list[str]) -> list[str]:
    """Onboarded repos across `owners`, in owner-then-listing order (bash loop order)."""
    repos: list[str] = []
    for owner in owners:
        for repo in _list_owner_repos(owner):
            if _is_onboarded(repo):
                repos.append(repo)
    return repos


def _enable_one(repo: str, endpoint: str) -> bool:
    """PUT one security endpoint; True on success. Mirrors `gh api -X PUT … --silent`."""
    try:
        gh.rest(f"repos/{repo}/{endpoint}", method="PUT")
        return True
    except gh.GhError:
        return False


def main(argv: list[str]) -> int:
    try:
        values, _ = cli.parse(
            argv,
            [
                cli.Opt("--owners", takes_value=True, default="arthur-debert,lex-fmt"),
                cli.Opt("--repos", takes_value=True, default=""),
                cli.Opt("--dry-run"),
            ],
            doc=_usage_block(),
        )
    except SystemExit as exc:
        return int(exc.code or 0)

    owners = values["owners"]
    explicit_repos = values["repos"] or ""
    dry_run = bool(values["dry-run"])

    if explicit_repos:
        repos = explicit_repos.split(",")
    else:
        print(f"discovering onboarded repos in: {owners}")
        repos = discover_repos([o for o in owners.split(",") if o])

    if not repos:
        print("no onboarded repos found", file=sys.stderr)
        return 1

    print(f"found {len(repos)} repo(s):")
    for repo in repos:
        print(f"  {repo}")
    print()

    failed = 0
    for repo in repos:
        print(f"  {repo}")
        if dry_run:
            print("    [dry] PUT vulnerability-alerts")
            print("    [dry] PUT automated-security-fixes")
            continue
        fail = 0
        if _enable_one(repo, "vulnerability-alerts"):
            print("    ok   alerts")
        else:
            print("    FAIL alerts")
            fail += 1
        if _enable_one(repo, "automated-security-fixes"):
            print("    ok   automated-fixes")
        else:
            print("    FAIL automated-fixes")
            fail += 1
        if fail > 0:
            failed += 1

    print()
    if failed > 0:
        print(f"summary: {len(repos)} repos, {failed} with failures", file=sys.stderr)
        return 1
    print(f"summary: {len(repos)} repos, alerts + automated fixes enabled")
    return 0
