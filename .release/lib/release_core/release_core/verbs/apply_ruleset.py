"""apply-ruleset — apply the canonical main-branch ruleset to a GitHub repo.

Usage:
  apply-ruleset [--dry-run] [--checks check1,check2,...]

Auto-detects:
  - repo from current git/gh context (gh repo view)
  - required checks: actual job names from the most recent default-branch
    run of each PR-trigger workflow (excluding copilot-review.yml — it
    isn't a gate). This catches matrix expansion (e.g. "Build
    (aarch64-apple-darwin)") and `name:` overrides that yq can't see.
    Falls back to yq job IDs if no runs exist yet.
Override checks with --checks (comma-separated). Use --dry-run to print the
payload without sending it.

Shell→Python migration (docs/proposals/shell-to-python.md): the ruleset payload
that was built with `yq -o json | jq` is now constructed as a Python dict and
PUT/POSTed via gh.rest(..., method=, body=) (the `gh api --input -` seam). The
payload is byte-identical to the old jq output, and the three stdout lines
(repo / ruleset / checks) plus the dry-run payload dump and exit codes are
preserved byte-for-byte.
"""

from __future__ import annotations

import json
import os
import sys

from .. import cli, gh, yamlio

USAGE = __doc__ or ""

# Workflow basenames that are not PR gates and must be excluded.
_NON_GATE_WORKFLOWS = ("copilot-review.yml", "copilot-review.yaml")


def _usage_block() -> str:
    """The bash `--help` block: lines 2..first-blank with the leading `# ` gone.

    The bash printed `sed -n '2,/^$/p' "$0" | sed -E 's/^# ?//'`, i.e. the full
    leading comment block. We reproduce it from the docstring, stopping at the
    Shell→Python migration note (which has no bash counterpart).
    """
    lines = USAGE.strip("\n").splitlines()
    out: list[str] = []
    for line in lines:
        if line.startswith("Shell→Python migration"):
            break
        out.append(line)
    return "\n".join(out).rstrip("\n")


# --------------------------------------------------------------------------
# Pure helpers — fixture-tested, no network.
# --------------------------------------------------------------------------


def workflow_triggers(workflow: object) -> list[str]:
    """The `on:` triggers of a parsed workflow doc, normalized to a flat list.

    Mirrors the bash jq: a string `on:` → `[on]`; an array → itself; an object
    → its keys; anything else → `[]`.
    """
    if not isinstance(workflow, dict):
        return []
    on = workflow.get("on")
    if isinstance(on, str):
        return [on]
    if isinstance(on, list):
        return [str(x) for x in on]
    if isinstance(on, dict):
        return list(on.keys())
    return []


def is_pr_workflow(workflow: object) -> bool:
    """True if the workflow is triggered on `pull_request`."""
    return "pull_request" in workflow_triggers(workflow)


def checks_json(checks: list[str]) -> list[dict]:
    """Map a list of check names → the `required_status_checks` array.

    Replaces `jq -R 'select(. != "") | {context: .}' | jq -s .`: drop empties,
    wrap each as `{"context": name}`, preserving order.
    """
    return [{"context": name} for name in checks if name != ""]


def build_payload(template: dict, checks: list[str]) -> dict:
    """Inject `checks` into the `required_status_checks` rule of `template`.

    Byte-for-byte equivalent of the bash jq:
        .rules |= map(if .type == "required_status_checks"
                      then .parameters.required_status_checks = $c else . end)
    `template` is the parsed main-protection.json.tmpl; this mutates a copy so
    the rest of the rules/conditions/bypass_actors are carried through verbatim.
    """
    import copy

    body = copy.deepcopy(template)
    contexts = checks_json(checks)
    for rule in body.get("rules", []):
        if isinstance(rule, dict) and rule.get("type") == "required_status_checks":
            rule.setdefault("parameters", {})["required_status_checks"] = contexts
    return body


def _existing_ruleset_id(rulesets: object, name: str) -> int | None:
    """First ruleset id whose name matches, or None. Mirrors the bash head -1."""
    for rs in rulesets or []:
        if isinstance(rs, dict) and rs.get("name") == name:
            return rs.get("id")
    return None


# --------------------------------------------------------------------------
# gh / fs boundary — check discovery.
# --------------------------------------------------------------------------


def _pr_workflow_paths(workflows_dir: str) -> list[str]:
    """Relative .github/workflows/<name> paths of PR-triggered, non-gate workflows.

    Globs *.yml then *.yaml (matching the bash loop order), parses each via yq,
    and keeps the ones triggered on pull_request.
    """
    import glob

    paths: list[str] = []
    names: list[str] = []
    for ext in ("*.yml", "*.yaml"):
        names.extend(sorted(glob.glob(os.path.join(workflows_dir, ext))))
    for path in names:
        base = os.path.basename(path)
        if base in _NON_GATE_WORKFLOWS:
            continue
        try:
            doc = yamlio.load(path)
        except yamlio.YamlError:
            continue
        if is_pr_workflow(doc):
            paths.append(f".github/workflows/{base}")
    return paths


def _checks_from_runs(repo: str, default_branch: str, paths: list[str]) -> list[str]:
    """Actual job-run names from the latest default-branch run of each workflow.

    Sorted-unique (the bash `sort -u`). Mirrors the three gh hops: workflow id by
    path → latest run id on the default branch → that run's job names.
    """
    found: set[str] = set()
    try:
        workflows_obj = gh.rest(f"repos/{repo}/actions/workflows")
    except gh.GhError:
        workflows_obj = None
    by_path = {}
    if isinstance(workflows_obj, dict):
        for wf in workflows_obj.get("workflows") or []:
            if isinstance(wf, dict) and wf.get("path"):
                by_path[wf["path"]] = wf.get("id")
    for path in paths:
        wid = by_path.get(path)
        if not wid:
            continue
        try:
            runs_obj = gh.rest(
                f"repos/{repo}/actions/workflows/{wid}/runs?branch={default_branch}&per_page=1"
            )
        except gh.GhError:
            continue
        runs = runs_obj.get("workflow_runs") if isinstance(runs_obj, dict) else None
        if not runs:
            continue
        run_id = runs[0].get("id") if isinstance(runs[0], dict) else None
        if not run_id:
            continue
        try:
            jobs_obj = gh.rest(f"repos/{repo}/actions/runs/{run_id}/jobs", paginate=True)
        except gh.GhError:
            continue
        for job in jobs_obj or []:
            if isinstance(job, dict) and job.get("name"):
                found.add(job["name"])
    return sorted(found)


def _checks_from_yq(toplevel: str, paths: list[str]) -> list[str]:
    """Fallback: job IDs (yq `.jobs | keys`) across the workflows, sorted-unique."""
    found: set[str] = set()
    for path in paths:
        try:
            doc = yamlio.load(os.path.join(toplevel, path))
        except yamlio.YamlError:
            continue
        if isinstance(doc, dict) and isinstance(doc.get("jobs"), dict):
            found.update(doc["jobs"].keys())
    return sorted(found)


def _current_repo() -> str:
    """`gh repo view --json nameWithOwner -q .nameWithOwner`."""
    return gh.repo_view(json_fields=["nameWithOwner"], q=".nameWithOwner")


def _load_template(release_root: str) -> dict:
    """Parse rulesets/main-protection.json.tmpl into a dict, or raise FileNotFoundError."""
    tmpl = os.path.join(release_root, "rulesets", "main-protection.json.tmpl")
    if not os.path.isfile(tmpl):
        raise FileNotFoundError(tmpl)
    with open(tmpl, encoding="utf-8") as fh:
        return json.load(fh)


def _release_root() -> str:
    """The release/ checkout root, where rulesets/main-protection.json.tmpl lives.

    The verb lives at templates/commons/lib/release_core/release_core/verbs/, so
    the repo root is six parents up. (The bash resolved it from the shim's dir,
    bin/, but the template lives at <root>/rulesets either way.)
    """
    here = os.path.dirname(os.path.realpath(__file__))
    return os.path.normpath(os.path.join(here, "..", "..", "..", "..", "..", ".."))


def main(argv: list[str]) -> int:
    try:
        values, _ = cli.parse(
            argv,
            [
                cli.Opt("--dry-run"),
                cli.Opt("--checks", takes_value=True, default=""),
            ],
            doc=_usage_block(),
        )
    except SystemExit as exc:
        return int(exc.code or 0)

    dry_run = bool(values["dry-run"])
    checks_override = values["checks"] or ""

    repo = _current_repo()

    if checks_override:
        # Override preserves the comma order (bash `tr ',' '\n'`); no sort.
        checks = checks_override.split(",")
    else:
        toplevel = gh.repo_root()
        workflows_dir = os.path.join(toplevel, ".github", "workflows")
        if not os.path.isdir(workflows_dir):
            print("no .github/workflows dir; pass --checks <names>", file=sys.stderr)
            return 1
        paths = _pr_workflow_paths(workflows_dir)
        default_branch = gh.rest(f"repos/{repo}")["default_branch"]
        checks = _checks_from_runs(repo, default_branch, paths)
        if not checks:
            checks = _checks_from_yq(toplevel, paths)

    # `checks_str` in bash was a newline list; an empty override or no-runs/no-yq
    # leaves it empty → the same error + exit 1.
    checks = [c for c in checks if c != ""]
    if not checks:
        print("no required checks could be determined; pass --checks <names>", file=sys.stderr)
        return 1

    try:
        template = _load_template(_release_root())
    except FileNotFoundError as exc:
        print(f"template not found: {exc}", file=sys.stderr)
        return 1

    body = build_payload(template, checks)
    name = body["name"]

    try:
        rulesets = gh.rest(f"repos/{repo}/rulesets")
    except gh.GhError:
        rulesets = None
    existing_id = _existing_ruleset_id(rulesets, name)

    print(f"repo:    {repo}")
    print(f"ruleset: {name} (existing id: {existing_id if existing_id is not None else 'none'})")
    print(f"checks:  {','.join(checks)}")

    if dry_run:
        print()
        print("--- payload (dry-run, not sent) ---")
        print(json.dumps(body, indent=2))
        return 0

    if existing_id is not None:
        gh.rest(f"repos/{repo}/rulesets/{existing_id}", method="PUT", body=body)
        print("updated")
    else:
        gh.rest(f"repos/{repo}/rulesets", method="POST", body=body)
        print("created")
    return 0
