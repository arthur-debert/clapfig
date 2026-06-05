"""install-release-token — install a release PAT as RELEASE_TOKEN across onboarded repos.

Why: on user-owned repos (and on org repos without org-level GitHub Actions
integration), GITHUB_TOKEN cannot bypass branch rulesets — `Integration`
actor bypass is rejected and `RepositoryRole` bypass only matches human
collaborators, not the github-actions[bot]. A Personal Access Token
authenticates as the owner (admin), which DOES match the admin RepositoryRole
bypass, so release workflows that push the version bump to default succeed.

Usage:
  install-release-token            # reads token from stdin
  pbpaste | install-release-token  # macOS clipboard
  install-release-token --owners arthur-debert,lex-fmt

Discovery: queries every repo under the given owners for a ruleset named
"main-branch-protection". Same logic apply-ruleset uses to find onboarded
repos.

Shell→Python migration: the gh-api+jq
discovery + the `gh secret set`/`gh secret list` set-then-verify loop moved
into Python (gh.rest / gh.secret_set / gh.secret_list, no jq). Token validation
still shells to `curl /user` (via proc, no shell string) because it must read
the X-OAuth-Scopes response header, which `gh api` does not surface. Stdout
lines, the required-scope contract, and exit codes are preserved byte-for-byte.
"""

from __future__ import annotations

import json
import re
import sys

from .. import cli, gh, proc

USAGE = __doc__ or ""

REQUIRED_SCOPES = ("repo", "read:org")


def _usage_block() -> str:
    """The docstring up to (not including) the Shell→Python migration note."""
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


# --------------------------------------------------------------------------
# Token validation — hit /user, verify auth + OAuth scopes (the X-OAuth-Scopes
# header, which `gh api` does not expose → curl, the same as the bash).
# --------------------------------------------------------------------------


def _curl_user(token: str) -> tuple[str, str, str]:
    """curl -D - .../user → (http_code, body, x-oauth-scopes header value).

    Returns the parsed response: dump headers + body, then split out the
    HTTP_CODE marker, the X-OAuth-Scopes header, and the JSON body. Mirrors the
    bash sed/awk/grep extraction.
    """
    result = proc.run(
        [
            "curl",
            "-fsSL",
            "-D",
            "-",
            "-H",
            f"Authorization: Bearer {token}",
            "https://api.github.com/user",
            "--write-out",
            "\nHTTP_CODE:%{http_code}\n",
        ],
        check=False,
    )
    # curl -f makes a >=400 response exit nonzero with no body; still parse what
    # we got from stdout (the --write-out HTTP_CODE line is emitted regardless).
    raw = result.stdout
    return _parse_curl_response(raw)


def _parse_curl_response(raw: str) -> tuple[str, str, str]:
    """Split a `curl -D -` dump into (http_code, json_body, scopes_header)."""
    lines = raw.splitlines()

    code = ""
    for line in lines:
        m = re.match(r"^HTTP_CODE:(.*)$", line)
        if m:
            code = m.group(1)  # tail -1 — last marker wins

    scopes_hdr = ""
    for line in lines:
        if line.lower().startswith("x-oauth-scopes:"):
            scopes_hdr = line.split(":", 1)[1]
            scopes_hdr = scopes_hdr.strip().rstrip("\r ")
            break  # head -1 — first match wins

    # Body: drop the HTTP_CODE marker, then everything after the blank line that
    # separates headers from body (awk: h=1 until first blank line, then print).
    body_lines: list[str] = []
    in_body = False
    for line in lines:
        if line.startswith("HTTP_CODE:"):
            continue
        if not in_body:
            if line == "" or line == "\r":
                in_body = True
            continue
        body_lines.append(line)
    body = "\n".join(body_lines)

    return code, body, scopes_hdr


def validate_token(code: str, body: str, scopes_hdr: str) -> tuple[bool, str, list[str]]:
    """Validate the /user response → (ok, error_message, info_lines).

    On success info_lines holds the two stdout lines the bash printed
    (authenticates-as + scopes). On failure error_message holds the multi-line
    stderr text and ok is False. Mirrors the bash precedence exactly.
    """
    login = ""
    try:
        obj = json.loads(body) if body.strip() else {}
        if isinstance(obj, dict):
            login = obj.get("login") or ""
    except json.JSONDecodeError:
        login = ""

    if code != "200" or not login:
        return False, f"error: token authentication failed (http {code})", []

    if not scopes_hdr:
        return (
            False,
            "error: token has no OAuth scopes (likely a fine-grained PAT).\n"
            f"       This script needs a classic PAT with: {' '.join(REQUIRED_SCOPES)}\n"
            "       Create one at https://github.com/settings/tokens/new",
            [],
        )

    have = {s.strip() for s in scopes_hdr.split(",")}
    missing = [s for s in REQUIRED_SCOPES if s not in have]
    if missing:
        return (
            False,
            f"error: token is missing required OAuth scope(s): {' '.join(missing)}\n"
            f"       Token has: {scopes_hdr}\n"
            "       Edit token scopes at https://github.com/settings/tokens",
            [],
        )

    return (
        True,
        "",
        [f"token authenticates as: {login}", f"token scopes: {scopes_hdr}"],
    )


# --------------------------------------------------------------------------
# Discovery — onboarded (main-branch-protection) repos.
# --------------------------------------------------------------------------


def _has_ruleset(repo: str) -> bool:
    """True if repo carries the main-branch-protection ruleset (tolerates per-repo errors)."""
    try:
        rulesets = gh.rest(f"repos/{repo}/rulesets")
    except gh.GhError:
        return False
    for r in rulesets or []:
        if isinstance(r, dict) and r.get("name") == "main-branch-protection":
            return True
    return False


def _list_repos(owner: str) -> list[str]:
    """`gh repo list OWNER --limit 200 --json nameWithOwner` → nameWithOwner list."""
    out = gh.repo_list(owner, limit=200, json_fields=["nameWithOwner"], jq=".[].nameWithOwner")
    return [line for line in out.splitlines() if line.strip()]


def discover_onboarded_repos(owners: list[str]) -> list[str]:
    """Repos under each owner that carry the main-branch-protection ruleset."""
    repos: list[str] = []
    for owner in owners:
        for repo in _list_repos(owner):
            if _has_ruleset(repo):
                repos.append(repo)
    return repos


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    opts = [cli.Opt("--owners", takes_value=True, default="arthur-debert,lex-fmt")]
    try:
        values, _ = cli.parse(argv, opts, positionals=(0, 0), doc=_usage_block())
    except SystemExit as exc:
        return int(exc.code or 0)

    owners = values["owners"]

    if sys.stdin.isatty():
        prog = "install-release-token"
        print(f"error: pipe the PAT to stdin (e.g. pbpaste | {prog})", file=sys.stderr)
        return 64

    # cat | tr -d '\n\r ' — strip all newlines, carriage returns, spaces.
    token = re.sub(r"[\n\r ]", "", sys.stdin.read())
    if not token:
        print("error: empty token on stdin", file=sys.stderr)
        return 64

    # --- validate ---
    code, body, scopes_hdr = _curl_user(token)
    ok, err, info = validate_token(code, body, scopes_hdr)
    if not ok:
        print(err, file=sys.stderr)
        return 1
    for line in info:
        print(line)

    # --- discovery ---
    print(f"discovering onboarded repos in: {owners}")
    repos = discover_onboarded_repos([o for o in owners.split(",") if o])

    if not repos:
        print(f"no onboarded repos found under: {owners}", file=sys.stderr)
        return 1

    print(f"found {len(repos)} repo(s):")
    for repo in repos:
        print(f"  {repo}")
    print()

    # --- set + verify ---
    failed = 0
    verified_ok = 0
    verified_missing = 0
    for repo in repos:
        try:
            gh.secret_set("RELEASE_TOKEN", token, repo=repo)
        except gh.GhError as exc:
            print(f"  FAIL {repo} — gh secret set: {exc}", file=sys.stderr)
            failed += 1
            continue

        # Verify by re-listing — gh secret set returns 0 but doesn't always
        # actually persist (write race, CI-hash mismatch).
        try:
            names = gh.secret_list(repo)
        except gh.GhError:
            names = []
        if "RELEASE_TOKEN" in names:
            print(f"  ok   {repo}")
            verified_ok += 1
        else:
            print(
                f"  FAIL {repo} — secret set returned 0 but RELEASE_TOKEN absent on re-list",
                file=sys.stderr,
            )
            failed += 1
            verified_missing += 1

    print()
    print(f"summary: {len(repos)} repos, {verified_ok} verified set, {failed} failure(s)")
    if verified_missing > 0:
        print(
            f"  — {verified_missing} repo(s) returned success but secret didn't persist; "
            "investigate auth/permissions",
            file=sys.stderr,
        )
    if failed > 0:
        return 1
    return 0
