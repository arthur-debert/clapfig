"""install-release-secrets — install rust-cli release secrets across onboarded rust repos.

Why: the `arthur-debert/release` rust-cli reusable workflow needs up
to 8 GH secrets to do its job (Apple sign+notarize, crates.io publish,
Homebrew tap push, optional npm publish for wasm-bindgen workspaces).
This script propagates them from canonical local sources so onboarding
a new project — or rotating secrets across all projects — is a single
command. Companion to `install-release-token` (which handles
RELEASE_TOKEN separately).

Sources:
  APPLE_CERTIFICATE_P12_BASE64  = base64(~/h/dotfiles/apple/auth/developerID_application.p12)
  APPLE_CERTIFICATE_PASSWORD    = cat ~/h/dotfiles/apple/auth/p12_password.txt
  ASC_API_KEY_BASE64            = base64(~/h/dotfiles/apple/auth/AuthKey_*.p8)
  ASC_API_KEY_ID                = parsed from p8 filename
  ASC_API_ISSUER_ID             = cat ~/h/dotfiles/apple/auth/asc_issuer_id.txt
  CRATES_IO_KEY                 = $CRATES_IO_KEY (env)
  HOMEBREW_TAP_TOKEN            = $HOMEBREW_TAP_TOKEN (env)
  NPM_TOKEN                     = $NPM_TOKEN (env, optional — only set
                                   when present, else warned and skipped)

Usage:
  install-release-secrets                          # auto-discover rust repos
  install-release-secrets --owners arthur-debert
  install-release-secrets --repos arthur-debert/dodot,lex-fmt/lex
  install-release-secrets --dry-run

Discovery: queries every repo under the given owners that has the
`main-branch-protection` ruleset (same set install-release-token uses),
then filters to those with Cargo.toml at the repo root.

Shell→Python migration: the gh-api+jq
discovery and the `gh secret set` loop moved into Python (gh.rest /
gh.secret_set, no jq). The set-of-7-secrets contract, the optional NPM_TOKEN
8th slot, stdout lines, and exit codes are preserved byte-for-byte.
"""

from __future__ import annotations

import base64
import os
import re
import sys

from .. import cli, gh

USAGE = __doc__ or ""

# The 7 always-set secrets, in the order the bash set them. NPM_TOKEN is an
# optional 8th, set only when present in the env. If this set changes, the
# CLAUDE.md rule binds install-release-token + install-release-secrets +
# bin/install-release-secrets's docstring in lockstep — do not silently edit.
_P8_ID_RE = re.compile(r"^AuthKey_(.+)\.p8$")


class SourceError(RuntimeError):
    """A required secret source (file or env var) is missing/invalid."""


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
# Secret sourcing — read the canonical local files + env vars.
# --------------------------------------------------------------------------


def collect_secrets(auth_dir: str, env: dict[str, str]) -> tuple[list[tuple[str, str]], bool]:
    """Read every secret source → (ordered [(name, value)], npm_present).

    Mirrors the bash sourcing precisely: missing required files/env raise
    SourceError with the same message the bash printed to stderr. NPM_TOKEN is
    optional — when absent it is simply not included (the bool reports that so
    the caller can print the skip notice + adjust the summary count).
    """
    # --- Apple cert ---
    p12_file = os.path.join(auth_dir, "developerID_application.p12")
    p12_password_file = os.path.join(auth_dir, "p12_password.txt")
    if not os.path.isfile(p12_file):
        raise SourceError(f"missing: {p12_file}")
    if not os.path.isfile(p12_password_file):
        raise SourceError(_missing_password_message(p12_file, p12_password_file))

    # --- ASC API key ---
    p8_file = _find_p8(auth_dir)
    if p8_file is None:
        raise SourceError(f"missing: {os.path.join(auth_dir, 'AuthKey_*.p8')}")
    m = _P8_ID_RE.match(os.path.basename(p8_file))
    asc_key_id = m.group(1) if m else os.path.basename(p8_file)

    issuer_file = os.path.join(auth_dir, "asc_issuer_id.txt")
    if not os.path.isfile(issuer_file):
        raise SourceError(f"missing: {issuer_file}")

    # --- env-resident tokens ---
    crates_io_key = env.get("CRATES_IO_KEY") or ""
    if not crates_io_key:
        raise SourceError("env CRATES_IO_KEY not set")
    homebrew_tap_token = env.get("HOMEBREW_TAP_TOKEN") or ""
    if not homebrew_tap_token:
        raise SourceError("env HOMEBREW_TAP_TOKEN not set")

    # --- encode / read ---
    p12_b64 = _b64_file(p12_file)
    p8_b64 = _b64_file(p8_file)
    with open(p12_password_file, encoding="utf-8") as f:
        p12_password = f.read()
    with open(issuer_file, encoding="utf-8") as f:
        # tr -d '\n\r ' — strip newlines, carriage returns, spaces.
        issuer_id = re.sub(r"[\n\r ]", "", f.read())

    secrets: list[tuple[str, str]] = [
        ("APPLE_CERTIFICATE_P12_BASE64", p12_b64),
        ("APPLE_CERTIFICATE_PASSWORD", p12_password),
        ("ASC_API_KEY_BASE64", p8_b64),
        ("ASC_API_KEY_ID", asc_key_id),
        ("ASC_API_ISSUER_ID", issuer_id),
        ("CRATES_IO_KEY", crates_io_key),
        ("HOMEBREW_TAP_TOKEN", homebrew_tap_token),
    ]

    npm_token = env.get("NPM_TOKEN") or ""
    npm_present = bool(npm_token)
    if npm_present:
        secrets.append(("NPM_TOKEN", npm_token))

    return secrets, npm_present


def _find_p8(auth_dir: str) -> str | None:
    """First AuthKey_*.p8 under auth_dir (glob order = sorted, like the shell)."""
    try:
        names = sorted(os.listdir(auth_dir))
    except OSError:
        return None
    for name in names:
        if name.startswith("AuthKey_") and name.endswith(".p8"):
            return os.path.join(auth_dir, name)
    return None


def _b64_file(path: str) -> str:
    """base64(file) as a single line — matches `base64 -i FILE` output."""
    with open(path, "rb") as f:
        return base64.b64encode(f.read()).decode("ascii")


def _missing_password_message(p12_file: str, p12_password_file: str) -> str:
    return (
        f"missing: {p12_password_file}\n"
        "\n"
        f"This file should contain the password for {p12_file} on a single line.\n"
        "If lost, re-pack the cert with a fresh hex password:\n"
        "\n"
        "  openssl pkcs12 -legacy -in <source.p12> -out /tmp/cert.pem -nodes\n"
        "  PW=$(openssl rand -hex 16)\n"
        "  openssl pkcs12 -legacy -export -in <source.p12> -out "
        f"{p12_file} -password pass:$PW\n"
        f"  printf '%s' \"$PW\" > {p12_password_file}\n"
        f"  chmod 600 {p12_password_file}\n"
        "  rm /tmp/cert.pem\n"
        "\n"
        "…then re-run this script. Note: re-packing invalidates the password\n"
        "already set in every consumer repo, so plan to run this after re-pack."
    )


# --------------------------------------------------------------------------
# Discovery — onboarded (main-branch-protection) repos with a root Cargo.toml.
# --------------------------------------------------------------------------


def _has_ruleset(repo: str) -> bool:
    """True if repo carries the main-branch-protection ruleset.

    Tolerates per-repo failures (legacy/private repos w/o rulesets) the way the
    bash `gh api … 2>/dev/null || echo '[]'` did: an error → no ruleset.
    """
    try:
        rulesets = gh.rest(f"repos/{repo}/rulesets")
    except gh.GhError:
        return False
    for r in rulesets or []:
        if isinstance(r, dict) and r.get("name") == "main-branch-protection":
            return True
    return False


def _has_cargo_toml(repo: str) -> bool:
    """True if repo has Cargo.toml at its root (the rust-repo filter)."""
    try:
        return gh.rest(f"repos/{repo}/contents/Cargo.toml") is not None
    except gh.GhError:
        return False


def _list_repos(owner: str) -> list[str]:
    """`gh repo list OWNER --limit 200 --json nameWithOwner` → nameWithOwner list."""
    out = gh.repo_list(owner, limit=200, json_fields=["nameWithOwner"], jq=".[].nameWithOwner")
    return [line for line in out.splitlines() if line.strip()]


def discover_rust_repos(owners: list[str]) -> list[str]:
    """Onboarded rust repos under each owner (ruleset + root Cargo.toml)."""
    repos: list[str] = []
    for owner in owners:
        for repo in _list_repos(owner):
            if not _has_ruleset(repo):
                continue
            if _has_cargo_toml(repo):
                repos.append(repo)
    return repos


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    opts = [
        cli.Opt("--owners", takes_value=True, default="arthur-debert,lex-fmt"),
        cli.Opt("--repos", takes_value=True, default=""),
        cli.Opt(
            "--auth-dir",
            takes_value=True,
            default=os.path.join(os.path.expanduser("~"), "h", "dotfiles", "apple", "auth"),
        ),
        cli.Opt("--dry-run", default=False),
    ]
    try:
        values, _ = cli.parse(argv, opts, positionals=(0, 0), doc=_usage_block())
    except SystemExit as exc:
        return int(exc.code or 0)

    owners = values["owners"]
    explicit_repos = values["repos"]
    auth_dir = values["auth-dir"]
    dry_run = bool(values["dry-run"])

    # --- collect secret sources (fail fast, before any discovery) ---
    try:
        secrets, npm_present = collect_secrets(auth_dir, dict(os.environ))
    except SourceError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    # --- discovery ---
    if explicit_repos:
        repos = [r for r in explicit_repos.split(",") if r]
    else:
        print(f"discovering onboarded rust repos in: {owners}")
        repos = discover_rust_repos([o for o in owners.split(",") if o])

    if not repos:
        print("no rust repos found", file=sys.stderr)
        return 1

    print(f"found {len(repos)} repo(s):")
    for repo in repos:
        print(f"  {repo}")
    print()

    if not npm_present:
        print(
            "note: NPM_TOKEN not in env — skipping that secret. Set it before "
            "re-running if any consumer uses the WASM/npm slot."
        )

    # --- set ---
    failed_repos = 0
    for repo in repos:
        print(f"  {repo}")
        fail = 0
        for name, value in secrets:
            if dry_run:
                print(f"    [dry] {name}")
                continue
            try:
                gh.secret_set(name, value, repo=repo)
            except gh.GhError:
                print(f"    FAIL {name}")
                fail += 1
        if fail > 0:
            failed_repos += 1

    print()
    if failed_repos > 0:
        print(f"summary: {len(repos)} repos, {failed_repos} with failures", file=sys.stderr)
        return 1
    secret_count = 8 if npm_present else 7
    print(f"summary: {len(repos)} repos, all {secret_count} secrets set")
    return 0
