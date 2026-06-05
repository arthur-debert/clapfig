"""audit-repo — audit a single repo against the portfolio "good state" baseline.

Usage:
  audit-repo                       # current git/gh repo
  audit-repo --repo owner/name
  audit-repo --json                # machine-readable
  audit-repo --quiet               # only print failing/warning rows

Exit codes:
  0  — all checks pass
  1  — at least one check failed
  2  — at least one check warned (no fails)
  64 — bad usage

Each check returns one of: PASS, FAIL, WARN, SKIP. Skips are for
checks that don't apply (e.g. private-module auth on a repo with no
go.mod). Warns are findings worth surfacing but not necessarily
blocking (e.g. dependabot.yml missing entirely vs. having an
off-policy ecosystem).

Checks performed:
  1. ruleset           — main-branch-protection ruleset present
  2. release_token     — RELEASE_TOKEN secret set
  3. copilot_review    — .github/workflows/copilot-review.yml on default branch
  4. codeowners        — .github/CODEOWNERS on default branch
  5. dep_security      — Dependabot vulnerability-alerts enabled
  6. dep_policy        — .github/dependabot.yml respects portfolio policy
                         (no npm/cargo/pip/etc. ecosystem; github-actions only)
  7. ci_main_green     — most recent default-branch CI run succeeded
  8. private_mod_auth  — if go.mod has private arthur-debert/* deps, workflows
                         configure git insteadOf with RELEASE_TOKEN

Conformance checks (WARN-only — don't fail; surface adoption gaps):
  9. release_sync       — .release-sync-state.yaml present (Component-model
                           adoption signal)
 10. scripts_inventory  — what's left in scripts/ beyond the canonical
                           setup-dev-env.sh + project extras
 11. workflows_canonical — count of workflows that are NOT thin callers of
                           arthur-debert/release/* (legacy / bespoke surface)
 12. ci_calls_bin_check    — does ANY workflow file (typically ci.yml /
                             test.yml) actually invoke `bin/check`? If not,
                             the Component-supplied canonical interface is
                             dead weight on disk — CI bypasses it.
 13. changelog_handling    — which changelog convention is in use:
                             fragment-dir (canonical, PASS) / single-file
                             (WARN, migrate per #201) / two-file (WARN) /
                             none (WARN) / mixed (FAIL).

Shell→Python migration: the base64/jq/grep
YAML gymnastics moved into Python (gh.rest → parsed dicts + base64-decoded file
bodies, no jq). The --json shape ({"repo","checks":[{"name","status","message"}]})
and the exit codes (1 fail / 2 warn / 0 ok) are preserved byte-for-byte;
audit-portfolio aggregates this verb's results.
"""

from __future__ import annotations

import base64
import binascii
import json
import re
import sys

from .. import gh

USAGE = __doc__ or ""

# Off-policy dependabot ecosystems: anything other than github-actions.
_ECOSYSTEM_RE = re.compile(r"^[ \t]*-[ \t]*package-ecosystem:[ \t]*(.+?)[ \t]*$", re.MULTILINE)
# go.mod private-dep refs (skip the repo's own `module …` line).
_GOMOD_DEP_RE = re.compile(r"^\s*(?:require\s+)?github\.com/arthur-debert/", re.MULTILINE)
_GOMOD_MODULE_RE = re.compile(r"^\s*module\s+")
# CHANGELOG.md single-file convention: a `## Unreleased` / `## [Unreleased]` head.
_UNRELEASED_RE = re.compile(r"^##[ \t]+\[?Unreleased\]?", re.MULTILINE)
# Workflow canonicality: thin caller of a release/ reusable workflow.
_CANONICAL_USE_RE = re.compile(r"uses:.*arthur-debert/release/\.github/workflows/")
# CI runs bin/check directly (umbrella), not bin/check-fmt alone.
_RUN_BIN_CHECK_RE = re.compile(r"^[ \t]*run:[ \t]*bin/check([ \t]|$)", re.MULTILINE)
# CI thin-calls a canonical release/ reusable that runs bin/check internally.
_CANONICAL_CALLEE_RE = re.compile(
    r"uses:[ \t]*arthur-debert/release/\.github/workflows/"
    r"(?:rust-ci|go-ci|electron-ci|tauri-ci|bats-e2e|mkdocs)\.yml"
)
# git insteadOf + RELEASE_TOKEN, the private-module auth signal.
_INSTEADOF_RE = re.compile(r"insteadOf.*github\.com")


def _usage_block() -> str:
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def _usage_error(msg: str) -> int:
    print(msg, file=sys.stderr)
    return 64


# --------------------------------------------------------------------------
# gh boundary helpers — the only place this verb touches GitHub.
# --------------------------------------------------------------------------


def _file_content(repo: str, path: str) -> str | None:
    """Decoded body of a repo file on its default branch, or None if absent.

    Replaces the bash `gh api .../contents/PATH --jq .content | base64 -d`.
    A 404 (file absent) surfaces as GhError → None; an empty file → "".
    """
    try:
        obj = gh.rest(f"repos/{repo}/contents/{path}")
    except gh.GhError:
        return None
    if not isinstance(obj, dict):
        return None
    content = obj.get("content")
    if content is None:
        return None
    try:
        return base64.b64decode(content).decode("utf-8", "replace")
    except (binascii.Error, ValueError):
        return None


def _dir_listing(repo: str, path: str) -> list[dict] | None:
    """File entries of a repo directory, or None if the dir is absent.

    Mirrors the bash `if type == "array"` guard: the contents API returns an
    array for a directory and an object (`{"message":"Not Found"}`) otherwise.
    """
    try:
        obj = gh.rest(f"repos/{repo}/contents/{path}")
    except gh.GhError:
        return None
    if not isinstance(obj, list):
        return None
    return obj


def _file_names(repo: str, path: str) -> list[str] | None:
    """Names of `type==file` entries in a repo directory, or None if absent."""
    listing = _dir_listing(repo, path)
    if listing is None:
        return None
    return [e["name"] for e in listing if e.get("type") == "file"]


# --------------------------------------------------------------------------
# Pure decision helpers — fixture-tested, no network.
# --------------------------------------------------------------------------


def off_policy_ecosystems(dependabot_yml: str) -> list[str]:
    """Sorted-unique package ecosystems other than github-actions.

    Replaces the grep|sed|tr|sort|uniq pipeline. Per portfolio policy only
    github-actions freshness is allowed; app-dep ecosystems violate it.
    """
    found = set()
    for raw in _ECOSYSTEM_RE.findall(dependabot_yml):
        value = raw.strip().strip("\"'")
        if value and value != "github-actions":
            found.add(value)
    return sorted(found)


def has_private_go_deps(gomod: str) -> bool:
    """True if go.mod requires arthur-debert/* deps (excluding its own module)."""
    for line in gomod.splitlines():
        if _GOMOD_MODULE_RE.match(line):
            continue
        if _GOMOD_DEP_RE.match(line):
            return True
    return False


def workflow_has_private_auth(body: str) -> bool:
    """True if a workflow body wires git insteadOf + RELEASE_TOKEN."""
    return bool(_INSTEADOF_RE.search(body)) and "RELEASE_TOKEN" in body


def classify_copilot_pointer(body: str) -> str:
    """Classify a copilot-review.yml body → 'release' | 'gh-dagentic' | 'unknown'."""
    if "arthur-debert/release/.github/workflows/copilot-review.yml@" in body:
        return "release"
    if "arthur-debert/gh-dagentic/.github/workflows/copilot-review.yml@" in body:
        return "gh-dagentic"
    return "unknown"


def classify_changelog(has_dir: bool, has_block: bool, has_two_file: bool) -> tuple[str, str]:
    """Map changelog signals → (status, message). Mirrors the bash precedence."""
    active = sum((has_dir, has_block, has_two_file))
    if active > 1:
        present = ""
        if has_dir:
            present += " fragment-dir"
        if has_block:
            present += " single-file"
        if has_two_file:
            present += " two-file"
        return "FAIL", f"mixed conventions:{present} — pick one (#201)"
    if has_dir:
        return "PASS", "fragment-dir (canonical)"
    if has_block:
        return "WARN", "single-file (## Unreleased) — migrate per #201"
    if has_two_file:
        return "WARN", "two-file (CHANGELOG_UNRELEASED.md) — migrate per #201"
    return "WARN", "none — no recognized changelog convention detected"


def parse_release_sync_state(body: str) -> tuple[str, str]:
    """Extract (sha, components-csv) from .release-sync-state.yaml text.

    Mirrors the bash awk/grep extraction (NOT a YAML parse — the bash walked the
    raw lines, and we preserve that to match byte-for-byte). `sha:` value and
    the `- ` list items under a top-level `components:` key.
    """
    sha = ""
    components: list[str] = []
    in_components = False
    for line in body.splitlines():
        if sha == "" and line.startswith("sha:"):
            parts = line.split()
            if len(parts) > 1:
                sha = parts[1]
        if line.startswith("components:"):
            in_components = True
            continue
        if in_components:
            if re.match(r"^[a-z_]+:", line):
                in_components = False
            elif re.match(r"^ *- ", line):
                components.append(re.sub(r"^ *- ", "", line))
    return sha, ",".join(components)


# --------------------------------------------------------------------------
# Checks — each appends (status, name, message) to `results`.
# --------------------------------------------------------------------------


def _record(results: list[tuple[str, str, str]], status: str, name: str, msg: str) -> None:
    results.append((status, name, msg))


def _check_ruleset(repo: str, results: list) -> None:
    try:
        rs = gh.rest(f"repos/{repo}/rulesets")
    except gh.GhError:
        _record(results, "FAIL", "ruleset", "API error querying rulesets")
        return
    ids = [
        r.get("id")
        for r in (rs or [])
        if isinstance(r, dict) and r.get("name") == "main-branch-protection"
    ]
    if ids and ids[0] is not None:
        _record(results, "PASS", "ruleset", f"id={ids[0]}")
    else:
        _record(
            results,
            "FAIL",
            "ruleset",
            "main-branch-protection ruleset not applied (run apply-ruleset)",
        )


def _check_release_token(repo: str, results: list) -> None:
    try:
        secrets = gh.rest(f"repos/{repo}/actions/secrets", paginate=True)
    except gh.GhError:
        _record(
            results, "FAIL", "release_token", "RELEASE_TOKEN not set (run install-release-token)"
        )
        return
    names = _secret_names(secrets)
    if "RELEASE_TOKEN" in names:
        _record(results, "PASS", "release_token", "set")
    else:
        _record(
            results, "FAIL", "release_token", "RELEASE_TOKEN not set (run install-release-token)"
        )


def _secret_names(secrets: object) -> set[str]:
    """Names from the /actions/secrets payload (paginated → list of dicts, or one dict)."""
    out: set[str] = set()
    items: list = []
    if isinstance(secrets, list):
        items = secrets
    elif isinstance(secrets, dict):
        items = secrets.get("secrets") or []
    for s in items:
        if isinstance(s, dict) and s.get("name"):
            out.add(s["name"])
    return out


def _check_copilot_review(repo: str, results: list) -> None:
    body = _file_content(repo, ".github/workflows/copilot-review.yml")
    if not body:
        _record(
            results,
            "FAIL",
            "copilot_review",
            ".github/workflows/copilot-review.yml missing on default branch",
        )
        return
    kind = classify_copilot_pointer(body)
    if kind == "release":
        _record(results, "PASS", "copilot_review", "present, points at release/@v1 (fixed)")
    elif kind == "gh-dagentic":
        _record(
            results,
            "FAIL",
            "copilot_review",
            "present but points at gh-dagentic (broken — silently no-ops Copilot attach)",
        )
    else:
        _record(results, "WARN", "copilot_review", "present but unknown reusable-workflow pointer")


def _check_codeowners(repo: str, results: list) -> None:
    if _exists(repo, ".github/CODEOWNERS") or _exists(repo, "CODEOWNERS"):
        _record(results, "PASS", "codeowners", "present")
    else:
        _record(results, "WARN", "codeowners", "CODEOWNERS missing")


def _exists(repo: str, path: str) -> bool:
    try:
        return gh.rest(f"repos/{repo}/contents/{path}") is not None
    except gh.GhError:
        return False


def _check_dep_security(repo: str, results: list) -> None:
    # `gh api repos/X/vulnerability-alerts` → 204 (empty) enabled, 404 disabled.
    # rest() returns None on a 204 success and raises GhError on a 404.
    try:
        gh.rest(f"repos/{repo}/vulnerability-alerts")
        _record(results, "PASS", "dep_security", "alerts enabled")
    except gh.GhError as exc:
        if "404" in str(exc):
            _record(
                results,
                "FAIL",
                "dep_security",
                "alerts disabled (run enable-dependabot-security)",
            )
        else:
            _record(results, "WARN", "dep_security", "unexpected error querying alerts")


def _check_dep_policy(repo: str, results: list) -> None:
    content = _file_content(repo, ".github/dependabot.yml")
    if not content:
        _record(
            results,
            "WARN",
            "dep_policy",
            "no .github/dependabot.yml (won't get GH Actions freshness)",
        )
        return
    off = off_policy_ecosystems(content)
    if not off:
        _record(results, "PASS", "dep_policy", "github-actions only")
    else:
        _record(results, "FAIL", "dep_policy", f"off-policy ecosystems present: {','.join(off)}")


def _check_ci_main_green(repo: str, results: list) -> None:
    try:
        repo_obj = gh.rest(f"repos/{repo}")
    except gh.GhError:
        repo_obj = None
    default_branch = repo_obj.get("default_branch") if isinstance(repo_obj, dict) else None
    if not default_branch:
        _record(results, "WARN", "ci_main_green", "could not determine default branch")
        return
    try:
        runs_obj = gh.rest(
            f"repos/{repo}/actions/runs?branch={default_branch}&event=push&per_page=20"
        )
    except gh.GhError:
        runs_obj = None
    runs = runs_obj.get("workflow_runs") if isinstance(runs_obj, dict) else None
    latest = next(
        (r for r in (runs or []) if isinstance(r, dict) and r.get("name") != "Copilot Review"),
        None,
    )
    if latest is None:
        _record(results, "WARN", "ci_main_green", f"no push runs found on {default_branch}")
        return
    conclusion = latest.get("conclusion")
    name = latest.get("name")
    if conclusion == "success":
        _record(
            results, "PASS", "ci_main_green", f"latest '{name}' run on {default_branch}: success"
        )
    elif conclusion == "failure":
        _record(
            results,
            "FAIL",
            "ci_main_green",
            f"latest '{name}' run on {default_branch}: failure (CI broken on main)",
        )
    elif conclusion == "cancelled":
        _record(
            results, "WARN", "ci_main_green", f"latest '{name}' run on {default_branch}: cancelled"
        )
    else:
        _record(
            results,
            "WARN",
            "ci_main_green",
            f"latest '{name}' run on {default_branch}: {conclusion}",
        )


def _check_private_mod_auth(repo: str, results: list) -> None:
    gomod = _file_content(repo, "go.mod")
    if not gomod:
        _record(results, "SKIP", "private_mod_auth", "no go.mod")
        return
    if not has_private_go_deps(gomod):
        _record(results, "SKIP", "private_mod_auth", "no arthur-debert/* go deps")
        return
    names = _file_names(repo, ".github/workflows") or []
    for f in names:
        if f == "copilot-review.yml":
            continue
        body = _file_content(repo, f".github/workflows/{f}")
        if body and workflow_has_private_auth(body):
            _record(results, "PASS", "private_mod_auth", "git insteadOf+RELEASE_TOKEN configured")
            return
    _record(
        results,
        "FAIL",
        "private_mod_auth",
        "arthur-debert/* deps in go.mod but no insteadOf+RELEASE_TOKEN in workflows",
    )


def _check_release_sync(repo: str, results: list) -> None:
    body = _file_content(repo, ".release-sync-state.yaml")
    if not body:
        _record(
            results,
            "WARN",
            "release_sync",
            "no .release-sync-state.yaml — Component model not adopted",
        )
        return
    sha, components = parse_release_sync_state(body)
    _record(
        results,
        "PASS",
        "release_sync",
        f"adopted (sha={sha[:7]}, components: {components or 'none'})",
    )


def _check_scripts_inventory(repo: str, results: list) -> None:
    listing = _file_names(repo, "scripts")
    if not listing:
        _record(results, "SKIP", "scripts_inventory", "no scripts/ dir on default branch")
        return
    extras = [n for n in listing if n != "setup-dev-env.sh"]
    if not extras:
        _record(results, "PASS", "scripts_inventory", "only setup-dev-env.sh (canonical)")
    else:
        _record(results, "WARN", "scripts_inventory", f"non-canonical: {','.join(extras)}")


def _check_workflows_canonical(repo: str, results: list) -> None:
    listing = _file_names(repo, ".github/workflows")
    if not listing:
        _record(results, "SKIP", "workflows_canonical", "no .github/workflows/ on default branch")
        return
    canonical = 0
    bespoke: list[str] = []
    total = 0
    for f in listing:
        total += 1
        body = _file_content(repo, f".github/workflows/{f}") or ""
        if _CANONICAL_USE_RE.search(body):
            canonical += 1
        else:
            bespoke.append(f)
    if not bespoke:
        _record(
            results,
            "PASS",
            "workflows_canonical",
            f"all {total} workflows are thin callers of release/",
        )
    else:
        _record(
            results,
            "WARN",
            "workflows_canonical",
            f"{canonical}/{total} canonical; bespoke: {', '.join(bespoke)}",
        )


def _check_ci_calls_bin_check(repo: str, results: list) -> None:
    sync_body = _file_content(repo, ".release-sync-state.yaml")
    if not sync_body:
        _record(results, "SKIP", "ci_calls_bin_check", "no Component model — N/A")
        return
    listing = _file_names(repo, ".github/workflows")
    if not listing:
        _record(results, "SKIP", "ci_calls_bin_check", "no .github/workflows/")
        return
    found: list[str] = []
    via_reusable: list[str] = []
    for f in listing:
        body = _file_content(repo, f".github/workflows/{f}") or ""
        if _RUN_BIN_CHECK_RE.search(body):
            found.append(f)
        elif _CANONICAL_CALLEE_RE.search(body):
            via_reusable.append(f)
    found_csv = ", ".join(found)
    via_csv = ", ".join(via_reusable)
    if found and via_reusable:
        _record(
            results, "PASS", "ci_calls_bin_check", f"direct: {found_csv}; via reusable: {via_csv}"
        )
    elif found:
        _record(results, "PASS", "ci_calls_bin_check", f"called in: {found_csv}")
    elif via_reusable:
        _record(
            results,
            "PASS",
            "ci_calls_bin_check",
            f"via release/ reusable workflow in: {via_csv}",
        )
    else:
        _record(
            results,
            "WARN",
            "ci_calls_bin_check",
            "Component model adopted but no workflow calls bin/check (CI duplicates invocation)",
        )


def _check_changelog_handling(repo: str, results: list) -> None:
    has_dir = _dir_listing(repo, "CHANGELOG") is not None
    body = _file_content(repo, "CHANGELOG.md")
    has_block = bool(body and _UNRELEASED_RE.search(body))
    has_two_file = _exists(repo, "CHANGELOG_UNRELEASED.md")
    status, msg = classify_changelog(has_dir, has_block, has_two_file)
    _record(results, status, "changelog_handling", msg)


_CHECKS = (
    _check_ruleset,
    _check_release_token,
    _check_copilot_review,
    _check_codeowners,
    _check_dep_security,
    _check_dep_policy,
    _check_ci_main_green,
    _check_private_mod_auth,
    _check_release_sync,
    _check_scripts_inventory,
    _check_workflows_canonical,
    _check_ci_calls_bin_check,
    _check_changelog_handling,
)


def audit(repo: str) -> list[tuple[str, str, str]]:
    """Run every check against `repo`, returning the (status, name, message) rows."""
    results: list[tuple[str, str, str]] = []
    for check in _CHECKS:
        check(repo, results)
    return results


def exit_code(results: list[tuple[str, str, str]]) -> int:
    """1 if any FAIL, else 2 if any WARN, else 0 — the bash precedence."""
    statuses = {status for status, _, _ in results}
    if "FAIL" in statuses:
        return 1
    if "WARN" in statuses:
        return 2
    return 0


def render_json(repo: str, results: list[tuple[str, str, str]]) -> str:
    """The --json shape: {"repo":…,"checks":[{"name","status","message"}]}."""
    checks = [{"name": name, "status": status, "message": msg} for status, name, msg in results]
    return json.dumps({"repo": repo, "checks": checks}, separators=(",", ":"))


def render_human(repo: str, results: list[tuple[str, str, str]], *, quiet: bool) -> str:
    lines = [repo]
    for status, name, msg in results:
        if quiet and status in ("PASS", "SKIP"):
            continue
        lines.append(f"  [{status:<4}] {name:<20} {msg}")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    repo = ""
    json_mode = False
    quiet = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--repo":
            if i + 1 >= len(argv):
                return _usage_error("--repo needs a value")
            i += 1
            repo = argv[i]
        elif arg == "--json":
            json_mode = True
        elif arg == "--quiet":
            quiet = True
        elif arg in ("-h", "--help"):
            print(_usage_block())
            return 0
        else:
            return _usage_error(f"unknown arg: {arg}")
        i += 1

    if not repo:
        repo = _current_repo()
        if not repo:
            print(
                "error: not in a gh-recognized repo and --repo not given",
                file=sys.stderr,
            )
            return 64

    results = audit(repo)

    if json_mode:
        print(render_json(repo, results))
    else:
        print(render_human(repo, results, quiet=quiet))

    return exit_code(results)


def _current_repo() -> str:
    """`gh repo view --json nameWithOwner -q .nameWithOwner`, '' on failure."""
    from .. import gh

    result = gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner", check=False)
    return result.stdout.strip() if result.returncode == 0 else ""
