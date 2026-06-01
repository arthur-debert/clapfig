"""done-check — fleet-enforcement gate for the "pilot-running" state contract.

Per release/ README §States — the contract: a (Stack, Repo) combo is
*pilot-running* when every applicable verb's local entry point + CI run is
green AND the most recent release was cut by the canonical `<stack>.yml@v1`
workflow. The adoption matrix (README) is hand-curated today, easy to drift;
this tool replaces that with a programmatic read of GitHub state.

Usage:
  done-check                       # current git/gh repo
  done-check --repo owner/name
  done-check --json                # machine-readable
  done-check --quiet               # only print failing rows

Exit codes:
  0   — pilot-running (every applicable verb is PASS)
  1   — not pilot-running (one or more verbs are FAIL or missing)
  2   — only WARN rows (e.g. release column unverified because the
        consumer has no releases yet — not a hard fail for new repos)
  64  — bad usage
  65  — could not detect stack

What it checks per applicable verb:
  L = local: `bin/<verb>` exists on default branch (the file IS the
      wiring signal; +x and actual exec are CI's job to gate, not
      done-check's — keep this fast enough to sweep a fleet from
      a single host)
  C = CI:    most recent default-branch run of the consumer's CI
      workflow (`ci.yml` by convention) succeeded
  release = the most recent GH release's creating workflow run came
      from `<stack>.yml@v1` (not a bespoke release.yml)

What's deliberately out of scope:
  - actually executing `bin/<verb>` (too heavy for fleet sweeps; CI
    gates this for real on every PR)
  - the "e2e (L/C)" column for stacks without an e2e flow
  - per-component checks (audit-repo covers those)

Shell→Python migration (docs/proposals/shell-to-python.md): the grep/sed/awk
release.yml parsing + jq CI/run aggregation moved into Python over gh.rest's
parsed dicts (no jq). Raw workflow-file bodies are read via the contents API +
base64 decode rather than the `Accept: vnd.github.raw` header, so the existing
gh.rest contract suffices with no new gh.py helper. Stdout (the table + the
--json object), exit codes, and the per-verb verdicts are preserved.
"""

from __future__ import annotations

import base64
import json
import re
import sys

from .. import gh

USAGE = __doc__ or ""

# ------------------------------------------------------------------
# Stack ↔ workflow-name ↔ applicable-verbs tables.
#
# Hardcoded for v1 (mirrors the bash case statements) — once
# templates/<stack>/manifest.yaml grows a `verbs:` + `release-workflow:`
# key, switch to reading that.
# ------------------------------------------------------------------
_WORKFLOW_TO_STACK = {
    "rust-cli.yml": "rust-cli",
    "rust-lib.yml": "rust-lib",
    "electron-app.yml": "electron-app",
    "tauri-app.yml": "tauri-app",
    "vscode-ext.yml": "vscode-ext",
    "nvim-plugin.yml": "nvim-plugin",
    "tree-sitter.yml": "tree-sitter",
    "zed-extension.yml": "zed-extension",
    "go-cli.yml": "go-cli",
    "gh-action.yml": "gh-action",
}

_VERBS_FOR_STACK = {
    "rust-cli": ["check", "build", "release"],
    "rust-lib": ["check", "build", "release"],
    "electron-app": ["check", "build", "release", "e2e"],
    "tauri-app": ["check", "build", "release", "e2e"],
    "vscode-ext": ["check", "build", "release"],
    "nvim-plugin": ["check", "release"],
    "tree-sitter": ["check", "build", "release"],
    "zed-extension": ["check", "build", "release"],
    "go-cli": ["check", "build", "release"],
    "gh-action": ["check", "release"],
    "brew-tap": [],  # out-of-scope per #175
}

_RELEASE_WORKFLOW_FOR_STACK = {
    "rust-cli": "rust-cli.yml",
    "rust-lib": "rust-lib.yml",
    "electron-app": "electron-app.yml",
    "tauri-app": "tauri-app.yml",
    "vscode-ext": "vscode-ext.yml",
    "nvim-plugin": "nvim-plugin.yml",
    "tree-sitter": "tree-sitter.yml",
    "zed-extension": "zed-extension.yml",
    "go-cli": "go-cli.yml",
    "gh-action": "gh-action.yml",
}

# `arthur-debert/release/.github/workflows/<name>.yml` — the canonical uses-line.
_CANONICAL_USES_RE = re.compile(r"arthur-debert/release/\.github/workflows/([a-z-]+\.yml)")
# `./.github/workflows/<name>.yml` — release/'s own self-call.
_SELFCALL_USES_RE = re.compile(r"\./\.github/workflows/([a-z-]+\.yml)")


class StackError(RuntimeError):
    """Stack could not be detected (maps to done-check's exit 65)."""


# ------------------------------------------------------------------
# gh boundary helpers — all return parsed dicts / decoded text; no jq.
# ------------------------------------------------------------------
def _raw_contents(repo: str, path: str, *, ref: str | None = None) -> str | None:
    """Fetch a file's body via the contents API, base64-decoded.

    The bash used `Accept: application/vnd.github.raw`; gh.rest always parses
    JSON, so instead we take the default JSON contents response
    ({"content": "<base64>", "encoding": "base64"}) and decode it in Python.
    Returns None when the file is absent or unreadable (the bash `|| rel_yml=""`).
    """
    api = f"repos/{repo}/contents/{path}"
    if ref:
        api += f"?ref={ref}"
    try:
        obj = gh.rest(api)
    except gh.GhError:
        return None
    if not isinstance(obj, dict):
        return None
    content = obj.get("content")
    if content is None:
        return None
    try:
        return base64.b64decode(content).decode("utf-8", errors="replace")
    except (ValueError, TypeError):
        return None


def _root_names(repo: str) -> list[str] | None:
    """Names of the entries at the repo root (paginated). None on read failure."""
    try:
        entries = gh.rest(f"repos/{repo}/contents", paginate=True)
    except gh.GhError:
        return None
    if not isinstance(entries, list):
        return None
    return [e["name"] for e in entries if isinstance(e, dict) and "name" in e]


def _file_exists(repo: str, path: str, *, ref: str | None = None) -> bool:
    """True iff the contents API returns a hit for path (the bash `gh api … >/dev/null`)."""
    api = f"repos/{repo}/contents/{path}"
    if ref:
        api += f"?ref={ref}"
    try:
        gh.rest(api)
    except gh.GhError:
        return False
    return True


# ------------------------------------------------------------------
# Stack detection.
#
# Primary: read the consumer's release.yml + match its `uses:` line for
# arthur-debert/release/.github/workflows/<NAME>.yml (or release/'s own
# self-call). Fallback: filesystem heuristics (matches bin/detect-kind).
# ------------------------------------------------------------------
def _workflow_name_from_release_yml(rel_yml: str) -> str | None:
    """The first canonical (or self-call) workflow filename referenced, or None."""
    m = _CANONICAL_USES_RE.search(rel_yml)
    if m:
        return m.group(1)
    m = _SELFCALL_USES_RE.search(rel_yml)
    if m:
        return m.group(1)
    return None


def detect_stack(repo: str) -> str:
    """Detect the consumer's stack from GitHub state. Raises StackError if undetermined."""
    rel_yml = _raw_contents(repo, ".github/workflows/release.yml")
    if rel_yml:
        wf_name = _workflow_name_from_release_yml(rel_yml)
        if wf_name and wf_name in _WORKFLOW_TO_STACK:
            return _WORKFLOW_TO_STACK[wf_name]

    # Fallback: filesystem heuristics, mirroring bin/detect-kind's precedence.
    names = _root_names(repo)
    if names is None:
        raise StackError("could not read repo contents (auth? rate limit?)")
    nameset = set(names)

    if "Formula" in nameset or "Casks" in nameset:
        return "brew-tap"
    if "grammar.js" in nameset:
        return "tree-sitter"
    if _file_exists(repo, "src-tauri/Cargo.toml") and "package.json" in nameset:
        return "tauri-app"
    if "extension.toml" in nameset and "Cargo.toml" in nameset:
        return "zed-extension"
    if "package.json" in nameset:
        pkg = _raw_contents(repo, "package.json") or ""
        if re.search(r'"electron"|"electron-builder"', pkg):
            return "electron-app"
        if re.search(r'"@vscode/vsce"|"vsce"', pkg):
            return "vscode-ext"
    if "Cargo.toml" in nameset:
        # Without release.yml to disambiguate, default to rust-lib (rust-cli
        # gets the discriminator from its release.yml above).
        return "rust-lib"
    if "go.mod" in nameset:
        return "go-cli"
    if "lua" in nameset or "plugin" in nameset:
        return "nvim-plugin"
    if "action.yml" in nameset or "action.yaml" in nameset:
        return "gh-action"
    raise StackError("could not detect stack")


# ------------------------------------------------------------------
# Per-verb checks. Each returns "STATE|message" mirroring the bash.
# ------------------------------------------------------------------
def check_local(repo: str, verb: str) -> str:
    """bin/<verb> exists on default branch. The file is the wiring signal."""
    if _file_exists(repo, f"bin/{verb}"):
        return f"PASS|bin/{verb}"
    return f"FAIL|bin/{verb} missing"


def _default_branch(repo: str) -> str:
    try:
        obj = gh.rest(f"repos/{repo}")
    except gh.GhError:
        return "main"
    if isinstance(obj, dict) and obj.get("default_branch"):
        return obj["default_branch"]
    return "main"


def check_ci(repo: str) -> str:
    """Most recent COMPLETED push run on ci.yml (then test.yml) on default branch."""
    branch = _default_branch(repo)
    for wf in ("ci.yml", "test.yml"):
        try:
            runs = gh.rest(
                f"repos/{repo}/actions/workflows/{wf}/runs?branch={branch}&event=push&per_page=10"
            )
        except gh.GhError:
            continue
        if not isinstance(runs, dict) or (runs.get("total_count") or 0) == 0:
            continue
        completed = [r for r in (runs.get("workflow_runs") or []) if r.get("status") == "completed"]
        if not completed:
            return f"WARN|{wf} has runs but none completed (all queued/in-progress)"
        conclusion = completed[0].get("conclusion") or ""
        if conclusion == "success":
            return f"PASS|{wf}"
        return f"FAIL|{wf} last completed run = {conclusion}"
    return f"WARN|no ci.yml or test.yml runs found on {branch}"


def check_release(repo: str, stack: str) -> str:
    """Latest release was cut by <stack>.yml@v1 (canonical or release/'s self-call)."""
    stack_wf = _RELEASE_WORKFLOW_FOR_STACK.get(stack)
    if stack_wf is None:
        return "SKIP|stack has no release workflow"

    # releases/latest = the GH-marked "Latest" (skips drafts + prereleases).
    latest_tag = ""
    try:
        latest = gh.rest(f"repos/{repo}/releases/latest")
        if isinstance(latest, dict):
            latest_tag = latest.get("tag_name") or ""
    except gh.GhError:
        latest_tag = ""
    if not latest_tag:
        # Fallback for repos that have only prereleases.
        try:
            rels = gh.rest(f"repos/{repo}/releases?per_page=1")
            if isinstance(rels, list) and rels and isinstance(rels[0], dict):
                latest_tag = rels[0].get("tag_name") or ""
        except gh.GhError:
            latest_tag = ""
    if not latest_tag:
        return "WARN|no releases — consumer has not shipped through canonical yet"

    rel_yml = _raw_contents(repo, ".github/workflows/release.yml", ref=latest_tag)
    if not rel_yml:
        return f"FAIL|no release.yml at tag {latest_tag} (release source unknown)"

    stack_wf_re = re.escape(stack_wf)
    if re.search(rf"uses:[ \t]+arthur-debert/release/\.github/workflows/{stack_wf_re}@", rel_yml):
        return f"PASS|{latest_tag} via {stack_wf}"
    # release/ itself's release.yml uses gh-action.yml@v1 via its OWN reusable
    # workflow — still counts as canonical.
    if re.search(rf"uses:[ \t]+\./\.github/workflows/{stack_wf_re}", rel_yml):
        return f"PASS|{latest_tag} via self-call {stack_wf}"
    return f"FAIL|{latest_tag} was NOT cut by {stack_wf}@v1 (release.yml is bespoke)"


# ------------------------------------------------------------------
# Aggregation. Pure over the four check_* results — the pytest oracle.
# ------------------------------------------------------------------
def aggregate(repo: str, stack: str, ci_result: str, per_verb: dict[str, str]) -> dict:
    """Build the result rows + overall state from precomputed check outputs.

    per_verb maps each applicable verb to either:
      - "release" → its check_release "STATE|msg" string
      - other verbs → their check_local "STATE|msg" string
    ci_result is the single shared check_ci "STATE|msg" (same answer per verb).

    Returns {repo, stack, state, exit_code, rows:[{verb,state,local,ci,msg}]}.
    Mirrors the bash main loop + exit-code policy byte-for-byte.
    """
    verbs = _VERBS_FOR_STACK[stack]
    ci_state, _, ci_msg = ci_result.partition("|")

    rows: list[dict] = []
    any_fail = False
    any_warn = False

    for verb in verbs:
        if verb == "release":
            rel_state, _, rel_msg = per_verb["release"].partition("|")
            # The local column for `release` reflects bin/release's presence,
            # carried in per_verb under the "release:local" key.
            local_for_release = per_verb["release:local"]
            local_state, _, local_msg = local_for_release.partition("|")
            rows.append(
                {
                    "verb": verb,
                    "state": rel_state,
                    "local": local_state,
                    "ci": "(n/a)",
                    "msg": f"{rel_msg} [local: {local_msg}]",
                }
            )
            any_fail = any_fail or rel_state == "FAIL"
            any_warn = any_warn or rel_state == "WARN"
        else:
            local_state, _, local_msg = per_verb[verb].partition("|")
            if local_state == "PASS" and ci_state == "PASS":
                rows.append(
                    {
                        "verb": verb,
                        "state": "PASS",
                        "local": local_state,
                        "ci": ci_state,
                        "msg": "ok",
                    }
                )
            elif local_state == "FAIL" or ci_state == "FAIL":
                rows.append(
                    {
                        "verb": verb,
                        "state": "FAIL",
                        "local": local_state,
                        "ci": ci_state,
                        "msg": f"local: {local_msg}; ci: {ci_msg}",
                    }
                )
                any_fail = True
            else:
                rows.append(
                    {
                        "verb": verb,
                        "state": "WARN",
                        "local": local_state,
                        "ci": ci_state,
                        "msg": f"local: {local_msg}; ci: {ci_msg}",
                    }
                )
                any_warn = True

    if any_fail:
        overall = "implemented"
        exit_code = 1
    elif any_warn:
        overall = "implemented+warnings"
        exit_code = 2
    else:
        overall = "pilot-running"
        exit_code = 0

    return {"repo": repo, "stack": stack, "state": overall, "exit_code": exit_code, "rows": rows}


# ------------------------------------------------------------------
# Emitters. Byte-for-byte with the bash table / JSON.
# ------------------------------------------------------------------
def render_json(result: dict) -> str:
    """Compact JSON object, byte-for-byte with the bash printf/jq emitter.

    The bash built ``{"repo":"…","stack":"…","state":"…","verbs":[…]}`` with no
    inter-token spaces; ``separators=(",", ":")`` reproduces that exactly, and
    json.dumps escapes ``msg`` the same way the old ``jq -Rn '$s'`` did.
    """
    obj = {
        "repo": result["repo"],
        "stack": result["stack"],
        "state": result["state"],
        "verbs": [
            {
                "verb": r["verb"],
                "state": r["state"],
                "local": r["local"],
                "ci": r["ci"],
                "msg": r["msg"],
            }
            for r in result["rows"]
        ],
    }
    return json.dumps(obj, separators=(",", ":"))


_TABLE_HEADER = (
    "| Verb    | local | CI    | result | notes                                                 |"
)
_TABLE_RULE = (
    "|---------|-------|-------|--------|-------------------------------------------------------|"
)


def render_table(result: dict, *, quiet: bool) -> str:
    lines = ["", _TABLE_HEADER, _TABLE_RULE]
    for r in result["rows"]:
        if quiet and r["state"] == "PASS":
            continue
        lines.append(
            f"| {r['verb']:<7} | {r['local']:<5} | {r['ci']:<5} | {r['state']:<6} | {r['msg']}"
        )
    lines.append("")
    lines.append(f"→ {result['repo']}/{result['stack']} : {result['state']}")
    lines.append("")
    return "\n".join(lines)


# ------------------------------------------------------------------
# Main
# ------------------------------------------------------------------
def _usage_block() -> str:
    out: list[str] = []
    for line in USAGE.strip("\n").splitlines():
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


def main(argv: list[str]) -> int:  # noqa: C901 — flat dispatch mirrors the bash modes
    repo = ""
    json_mode = False
    quiet = False

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--repo":
            if i + 1 >= len(argv):
                print("unknown arg: --repo", file=sys.stderr)
                return 64
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
            print(f"unknown arg: {arg}", file=sys.stderr)
            return 64
        i += 1

    if not repo:
        repo = _detect_current_repo()
        if not repo:
            print("could not detect repo (pass --repo owner/name)", file=sys.stderr)
            return 64

    print(f"Detecting stack for {repo}...")
    try:
        stack = detect_stack(repo)
    except StackError:
        print("could not detect stack", file=sys.stderr)
        return 65
    print(f"Stack: {stack}")

    if stack == "brew-tap":
        print(
            "brew-tap is explicitly out-of-scope (passive registry; no release "
            "semantics — see #175)"
        )
        if json_mode:
            print(
                json.dumps(
                    {"repo": repo, "stack": stack, "state": "out-of-scope", "verbs": []},
                    separators=(",", ":"),
                )
            )
        return 0

    if not _VERBS_FOR_STACK.get(stack):
        print(f"no verbs registered for stack={stack}", file=sys.stderr)
        return 1

    # One shared CI lookup — same answer for every non-release verb.
    ci_result = check_ci(repo)

    per_verb: dict[str, str] = {}
    for verb in _VERBS_FOR_STACK[stack]:
        if verb == "release":
            per_verb["release"] = check_release(repo, stack)
            per_verb["release:local"] = check_local(repo, verb)
        else:
            per_verb[verb] = check_local(repo, verb)

    result = aggregate(repo, stack, ci_result, per_verb)

    if json_mode:
        print(render_json(result))
    else:
        print(render_table(result, quiet=quiet))

    return result["exit_code"]


def _detect_current_repo() -> str:
    """`gh repo view --json nameWithOwner --jq .nameWithOwner`, '' on failure."""
    try:
        from .. import gh

        result = gh.repo_view(json_fields=["nameWithOwner"], jq=".nameWithOwner", check=False)
    except Exception:  # pragma: no cover — gh missing etc.
        return ""
    if result.returncode != 0:
        return ""
    return result.stdout.strip()
