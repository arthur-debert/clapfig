"""gh — the single boundary to GitHub/git: shell out, parse JSON with stdlib.

Mirrors the conventions of release_core.prstate.ghapi (the folded PR state
engine's gh boundary; release#459). The two boundaries are kept distinct on
purpose: prstate.ghapi is stdlib-only (it runs identically in CI / Cloud /
local), whereas this helper backs the verb layer. Why `gh` rather than a Python
client:
it is already provisioned in every environment release runs in, handles auth +
pagination, and speaks GraphQL. Keeping the boundary here means every migrated
verb is pure data transformation over the returned dicts — **no `jq`**.
"""

from __future__ import annotations

import json
import shutil

from . import proc


class GhError(RuntimeError):
    """A `gh` invocation failed, or `gh` is unavailable."""


def _gh(args: list[str], *, input_text: str | None = None) -> str:
    if shutil.which("gh") is None:
        raise GhError("`gh` CLI not found on PATH")
    result = proc.run(["gh", *args], input=input_text, check=False)
    if result.returncode != 0:
        raise GhError(f"gh {' '.join(args)} failed ({result.returncode}): {result.stderr.strip()}")
    return result.stdout


def _gh_raw(args: list[str], *, input_text: str | None = None):
    """`gh <args>` returning the raw CompletedProcess (check=False), without
    raising on nonzero. For call sites that inspect ``returncode`` and/or
    forward gh's own stdout/stderr verbatim (the porcelain wrappers below that
    mirror a bash `gh … ; rc=$?` rather than a `|| die`)."""
    return proc.run(["gh", *args], input=input_text, check=False)


def rest(
    path: str,
    *,
    method: str | None = None,
    fields: dict[str, str] | None = None,
    body: object | None = None,
    paginate: bool = False,
) -> object:
    """Call `gh api <path>` → parsed JSON (None on empty output). Raises GhError.

    ``body``, when given, is serialized to JSON and piped to `gh api --input -`
    — the only way to send an arbitrary nested request body (e.g. a ruleset
    payload) that the flat `-f key=value` ``fields`` form cannot express.
    ``fields`` and ``body`` are mutually exclusive.
    """
    if fields and body is not None:
        raise GhError("rest(): pass either fields= or body=, not both")
    args = ["api"]
    if method:
        args += ["-X", method]
    if paginate:
        args.append("--paginate")
    for key, value in (fields or {}).items():
        args += ["-f", f"{key}={value}"]
    input_text = None
    if body is not None:
        args += ["--input", "-"]
        input_text = json.dumps(body)
    args.append(path)
    output = _gh(args, input_text=input_text)
    if not output.strip():
        return None
    if paginate:
        return _merge_paginated(output)
    return json.loads(output)


def _merge_paginated(output: str) -> list:
    """`gh api --paginate` concatenates one JSON array per page; flatten them."""
    merged: list = []
    decoder = json.JSONDecoder()
    text = output.strip()
    idx = 0
    while idx < len(text):
        obj, end = decoder.raw_decode(text, idx)
        merged.extend(obj if isinstance(obj, list) else [obj])
        idx = end
        while idx < len(text) and text[idx] in " \n\r\t":
            idx += 1
    return merged


def issue_list(
    repo: str,
    *,
    state: str = "open",
    label: str | None = None,
    limit: int = 200,
    json_fields: list[str] | None = None,
) -> list:
    """`gh issue list --json …` → parsed list of issue dicts. Raises GhError.

    A thin wrapper over the `gh issue list` porcelain (not the raw REST search
    API): it handles the label/state filters and `--json` field selection the
    fleet-inbox verb needs, while keeping the gh boundary the single chokepoint.
    """
    args = ["issue", "list", "--repo", repo, "--state", state, "--limit", str(limit)]
    if label:
        args += ["--label", label]
    args += ["--json", ",".join(json_fields or [])]
    output = _gh(args)
    if not output.strip():
        return []
    return json.loads(output)


def secret_set(name: str, value: str, *, repo: str) -> None:
    """`gh secret set <name> -R <repo>` reading the value from stdin. Raises GhError.

    A helper (not a plain REST PUT) because setting an Actions secret requires
    libsodium-sealing the value against the repo's public key — `gh secret set`
    does that encryption transparently; a raw `gh api` call cannot.
    """
    _gh(["secret", "set", name, "-R", repo], input_text=value)


def secret_list(repo: str) -> list[str]:
    """`gh secret list -R <repo> --json name -q '.[].name'` → list of names. Raises GhError.

    Porcelain over the Actions-secrets surface, the read-side companion to
    :func:`secret_set` (it is paired with it to verify a set actually persisted).
    The structured `--json name -q '.[].name'` form yields one secret name per
    line with no header — robust against `gh secret list`'s human-table columns
    (and any future header row) that an ad-hoc whitespace split would misparse.
    """
    output = _gh(["secret", "list", "-R", repo, "--json", "name", "-q", ".[].name"])
    return [line.strip() for line in output.splitlines() if line.strip()]


def graphql(query: str, **variables: object) -> dict:
    """Run a GraphQL query/mutation; check payload['errors']; return the data dict."""
    args = ["api", "graphql", "-f", f"query={query}"]
    for key, value in variables.items():
        # Omit None entirely: an unprovided nullable GraphQL variable defaults
        # to null. Passing it through would send the literal string "None".
        if value is None:
            continue
        # -F type-infers ints/bools; -f forces a string (needed for ID! vars).
        flag = "-F" if isinstance(value, (int, bool)) else "-f"
        args += [flag, f"{key}={value}"]
    payload = json.loads(_gh(args))
    if payload.get("errors"):
        raise GhError(f"graphql errors: {payload['errors']}")
    return payload["data"]


def git(args: list[str], *, cwd: str | None = None, check: bool = True) -> str:
    """git porcelain via proc.out. e.g. ``git(['rev-parse', '--show-toplevel'])``."""
    return proc.out(["git", *args], cwd=cwd, check=check)


def repo_root(start: str | None = None) -> str:
    """``git rev-parse --show-toplevel``, resolved to a real path."""
    import os

    top = git(["rev-parse", "--show-toplevel"], cwd=start)
    return os.path.realpath(top)


def is_git_worktree(path: str) -> bool:
    """True iff ``path`` is inside a git working tree (a normal clone OR a
    linked worktree).

    Replaces the ``os.path.isdir(<path>/.git)`` guard, which wrongly rejected
    git worktrees: a linked worktree's ``.git`` is a *file* (a gitdir pointer),
    not a directory, so the isdir check reports a real worktree as "not a git
    clone". ``git rev-parse --is-inside-work-tree`` resolves the gitdir pointer
    natively and is true for both layouts. Returns False (never raises) when
    ``path`` is missing, not a repo, or git is unavailable — preserving the
    old guard's boolean contract."""
    import os

    if not os.path.isdir(path):
        return False
    try:
        result = proc.run(
            ["git", "-C", path, "rev-parse", "--is-inside-work-tree"],
            check=False,
        )
    except OSError:
        # git not installed / not on PATH — preserve the documented "never
        # raises, returns False when git is unavailable" contract.
        return False
    # exit 0 ⟺ git resolves a repo of some kind here (a clone root, a linked
    # worktree, or a $RELEASE_HOME we only read refs/trees from). A non-repo path
    # exits non-zero. We deliberately accept exit 0 rather than require "true"
    # output: this guard replaces the original os.path.isdir(.git) check, which
    # also accepted any git dir — narrowing it to checked-out work trees only
    # would over-reject (and breaks the $RELEASE_HOME guards' fixtures).
    return result.returncode == 0


# ──────────────────────────────────────────────────────────────────────────────
# git plumbing wrappers (added Phase 2 / s2p2-release-sync — ADDITIVE, flagged).
#
# release-sync reads managed file content + tree listings out of a ref in
# $RELEASE_HOME without checking it out. These mirror the EXACT `git -C <home>`
# plumbing the bash used (rev-parse --verify --quiet, fetch --prune, ls-tree,
# cat-file -e, show ref:path) so the offline BATS fixtures stay valid.
# `git_show_bytes` returns *raw bytes* (NOT proc.out, which strips) — managed
# blobs must be written byte-for-byte, never whitespace-trimmed.
# ──────────────────────────────────────────────────────────────────────────────


def git_rev_parse_verify(ref: str, *, cwd: str) -> bool:
    """`git -C <cwd> rev-parse --verify --quiet <ref>` → True iff the ref resolves.

    Mirrors the bash `if ! git rev-parse --verify --quiet <ref> >/dev/null`."""
    return (
        proc.run(
            ["git", "-C", cwd, "rev-parse", "--verify", "--quiet", ref],
            check=False,
        ).returncode
        == 0
    )


def git_rev_parse(ref: str, *, cwd: str) -> str:
    """`git -C <cwd> rev-parse <ref>` → the resolved object name (full SHA)."""
    return git(["-C", cwd, "rev-parse", ref])


def git_fetch_prune(*, cwd: str, remote: str = "origin") -> None:
    """`git -C <cwd> fetch --quiet --prune <remote>`."""
    git(["-C", cwd, "fetch", "--quiet", "--prune", remote])


def git_cat_file_exists(rev_path: str, *, cwd: str) -> bool:
    """`git -C <cwd> cat-file -e <rev>:<path>` → True iff the blob/tree exists.

    Mirrors the bash existence probe `git cat-file -e "$ref:$path" 2>/dev/null`."""
    return proc.run(["git", "-C", cwd, "cat-file", "-e", rev_path], check=False).returncode == 0


def git_ls_tree(
    ref: str,
    path: str,
    *,
    cwd: str,
    recursive: bool = False,
    dirs_only: bool = False,
    name_only: bool = False,
) -> str:
    """`git -C <cwd> ls-tree [-r] [-d] [--name-only] <ref> -- <path>` → raw stdout
    (NOT stripped — caller splits on newlines/tabs). Returns '' on failure (the
    bash piped through `|| true` / tolerated a missing tree)."""
    args = ["git", "-C", cwd, "ls-tree"]
    if recursive:
        args.append("-r")
    if dirs_only:
        args.append("-d")
    if name_only:
        args.append("--name-only")
    args += [ref, "--", path]
    res = proc.run(args, check=False)
    return res.stdout if res.returncode == 0 else ""


def git_tag_list_merged(pattern: str = "v*", *, cwd: str, sort: str = "-version:refname"):
    """`git -C <cwd> tag --list <pattern> --sort <sort> --merged HEAD` → raw
    CompletedProcess (check=False).

    Used by release-lex's generic should-release decision: enumerate the version
    tags reachable from HEAD, highest-version first. Returns the CompletedProcess
    so the caller can distinguish a benign empty result (rc 0, no tags) from a
    genuine git failure (rc != 0, e.g. corrupt repo / unborn HEAD) and surface
    the latter loudly rather than masking it as "no tags"."""
    return proc.run(
        ["git", "-C", cwd, "tag", "--list", pattern, "--sort", sort, "--merged", "HEAD"],
        check=False,
    )


def git_log_oneline(rev_range: str, *, cwd: str):
    """`git -C <cwd> --no-pager log --oneline <rev_range>` → raw CompletedProcess
    (check=False).

    The commit-count source for release-lex's should-release decision. Raw
    CompletedProcess so the caller surfaces a nonzero rc (bad ref / git error)
    loudly instead of reading an empty stdout as "nothing to release"."""
    return proc.run(
        ["git", "-C", cwd, "--no-pager", "log", "--oneline", rev_range],
        check=False,
    )


def git_current_branch(*, cwd: str) -> str | None:
    """`git -C <cwd> rev-parse --abbrev-ref HEAD` → the current branch name, or
    None on a detached HEAD (returns "HEAD") / failure (unborn branch, not a
    repo). Used by init's --commit/--push guard."""
    res = proc.run(["git", "-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"], check=False)
    if res.returncode != 0:
        return None
    name = res.stdout.strip()
    if not name or name == "HEAD":
        return None
    return name


def git_default_branch(*, cwd: str) -> str | None:
    """The repo's default branch name, or None if it can't be resolved.

    Reads `origin/HEAD`'s symbolic target (`refs/remotes/origin/HEAD` →
    `origin/<default>`); falls back to None when there is no `origin` remote or
    its HEAD is unset. The --push guard uses this to push ONLY when the local
    branch IS the default branch — never inventing 'main'/'master' by guess."""
    res = proc.run(
        ["git", "-C", cwd, "symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"],
        check=False,
    )
    if res.returncode != 0:
        return None
    ref = res.stdout.strip()  # e.g. "origin/main"
    prefix = "origin/"
    if ref.startswith(prefix):
        return ref[len(prefix) :]
    return ref or None


def git_is_clean(*, cwd: str, except_paths: list[str] | None = None) -> bool:
    """True iff `git status --porcelain` reports no changes other than the files
    in ``except_paths`` (each compared against the porcelain path column).

    The --push guard requires the working tree to be otherwise clean: we tolerate
    only the managed paths init itself just wrote (passed as ``except_paths``)."""
    res = proc.run(["git", "-C", cwd, "status", "--porcelain"], check=False)
    if res.returncode != 0:
        return False
    allowed = set(except_paths or [])
    for line in res.stdout.splitlines():
        if not line.strip():
            continue
        # Porcelain v1: 2 status chars, a space, then the path (possibly
        # "old -> new" for renames — managed config files are never renamed, so
        # the simple split is sufficient for the paths we care about).
        path = line[3:].strip()
        if path not in allowed:
            return False
    return True


def git_add(paths: list[str], *, cwd: str) -> None:
    """`git -C <cwd> add -- <paths>` — stage ONLY the given pathspecs. Never -A.
    Raises ProcError on failure."""
    if not paths:
        return
    git(["-C", cwd, "add", "--", *paths])


def git_commit_paths(paths: list[str], message: str, *, cwd: str) -> None:
    """`git -C <cwd> commit -m <message> -- <paths>` — commit ONLY the given
    pathspecs, leaving any other staged/unstaged changes untouched. Raises
    ProcError on failure (no commit created)."""
    git(["-C", cwd, "commit", "-m", message, "--", *paths])


def git_push_ff(branch: str, *, cwd: str, remote: str = "origin") -> None:
    """`git -C <cwd> push <remote> <branch>` — a plain (fast-forward-only) push.
    Never --force. Raises ProcError on rejection (e.g. non-fast-forward)."""
    git(["-C", cwd, "push", remote, branch])


def git_show_bytes(rev_path: str, *, cwd: str) -> bytes:
    """`git -C <cwd> show <rev>:<path>` → the blob's RAW bytes.

    Bytes, not text: managed content (scripts, YAML, the harness lib) must be
    materialized byte-for-byte. proc/`out` would strip and re-encode, so this
    runs subprocess directly in binary mode through the same no-shell discipline."""
    import subprocess

    res = subprocess.run(  # noqa: S603 — fixed argv, no shell
        ["git", "-C", cwd, "show", rev_path],
        capture_output=True,
        check=False,
    )
    if res.returncode != 0:
        raise GhError(
            f"git -C {cwd} show {rev_path} failed ({res.returncode}): "
            f"{res.stderr.decode('utf-8', 'replace').strip()}"
        )
    return res.stdout


# ──────────────────────────────────────────────────────────────────────────────
# Porcelain wrappers (Phase 1 chokepoint consolidation).
#
# Each mirrors the EXACT `gh` command line a verb call site used before this
# sweep — same subcommand, flags, --json field lists, -q/--jq queries — so both
# byte-for-byte behavior AND the offline BATS `gh` stubs stay valid. Return
# shapes match what each call site needs (raw stdout str, CompletedProcess for
# returncode/stream inspection, or bool for fire-and-forget), rather than
# forcing a parse the call site doesn't want.
# ──────────────────────────────────────────────────────────────────────────────


def repo_view(
    *,
    repo: str | None = None,
    json_fields: list[str] | None = None,
    jq: str | None = None,
    q: str | None = None,
    check: bool = True,
):
    """`gh repo view [repo] [--json …] [-q/--jq …]`.

    Used to resolve the current/target repo slug. ``jq=`` emits ``--jq`` and
    ``q=`` emits ``-q`` — the two spellings are NOT normalized because different
    call sites (and their BATS stubs) used one or the other verbatim.

    With ``check=True`` (default) returns stripped stdout, raising GhError on
    failure (mirrors ``proc.out``). With ``check=False`` returns the raw
    CompletedProcess so callers can degrade to '' on a nonzero exit.
    """
    args = ["repo", "view"]
    if repo is not None:
        args.append(repo)
    if json_fields is not None:
        args += ["--json", ",".join(json_fields)]
    if jq is not None:
        args += ["--jq", jq]
    if q is not None:
        args += ["-q", q]
    if check:
        return _gh(args).strip()
    return _gh_raw(args)


def repo_list(
    owner: str,
    *,
    limit: int = 200,
    json_fields: list[str] | None = None,
    jq: str | None = None,
    check: bool = True,
):
    """`gh repo list <owner> --limit <n> [--json …] [--jq …]`.

    Enumerates an owner's repos for the policy/onboarding sweeps. ``check=True``
    returns stripped stdout (``proc.out`` semantics); ``check=False`` returns the
    raw CompletedProcess so callers can swallow failure → []."""
    args = ["repo", "list", owner, "--limit", str(limit)]
    if json_fields is not None:
        args += ["--json", ",".join(json_fields)]
    if jq is not None:
        args += ["--jq", jq]
    if check:
        return _gh(args).strip()
    return _gh_raw(args)


def repo_clone(repo: str, dest: str):
    """`gh repo clone <repo> <dest>` → raw CompletedProcess (check=False).

    `gh repo clone` (not plain `git clone`) works in gh-authenticated sandboxes
    where git clone is restricted. Caller inspects ``returncode``."""
    return _gh_raw(["repo", "clone", repo, dest])


def pr_list(
    *,
    head: str | None = None,
    json_fields: list[str] | None = None,
    q: str | None = None,
):
    """`gh pr list [--head …] [--json …] [-q …]` → stripped stdout. Raises
    GhError on failure (call sites that swallow failure wrap this themselves)."""
    args = ["pr", "list"]
    if head is not None:
        args += ["--head", head]
    if json_fields is not None:
        args += ["--json", ",".join(json_fields)]
    if q is not None:
        args += ["-q", q]
    return _gh(args).strip()


def pr_create(
    *,
    repo: str | None = None,
    base: str | None = None,
    head: str | None = None,
    title: str,
    body: str,
):
    """`gh pr create [--repo …] [--base …] [--head …] --title … --body …`.

    Returns the raw CompletedProcess (check=False): call sites parse the PR
    URL/number from stdout and report their own success/FAILED line."""
    args = ["pr", "create"]
    if repo is not None:
        args += ["--repo", repo]
    if base is not None:
        args += ["--base", base]
    if head is not None:
        args += ["--head", head]
    args += ["--title", title, "--body", body]
    return _gh_raw(args)


def pr_merge(
    pr: str, *, repo: str, squash: bool = False, delete_branch: bool = False, admin: bool = False
) -> None:
    """`gh pr merge <pr> --repo … [--squash] [--delete-branch] [--admin]`.

    Streams gh's output (no capture) and raises GhError on nonzero — mirrors the
    original `subprocess.run(..., check=True)` whose failure aborts the release
    orchestration."""
    args = ["pr", "merge", pr, "--repo", repo]
    if squash:
        args.append("--squash")
    if delete_branch:
        args.append("--delete-branch")
    if admin:
        args.append("--admin")
    _gh_stream(args)


def pr_comment(target: str, *, body: str):
    """`gh pr comment <target> --body <body>` → raw CompletedProcess
    (check=False). Caller maps returncode → success/FAILED."""
    return _gh_raw(["pr", "comment", target, "--body", body])


def pr_close(pr: str, *, repo: str, delete_branch: bool = False, comment: str | None = None):
    """`gh pr close <pr> --repo … [--delete-branch] [--comment …]` → raw
    CompletedProcess (check=False), best-effort cleanup."""
    args = ["pr", "close", pr, "--repo", repo]
    if delete_branch:
        args.append("--delete-branch")
    if comment is not None:
        args += ["--comment", comment]
    return _gh_raw(args)


def run_list(
    *,
    repo: str | None = None,
    workflow: str | None = None,
    workflow_eq: str | None = None,
    branch: str | None = None,
    commit: str | None = None,
    limit: int | None = None,
    json_fields: list[str] | None = None,
    q: str | None = None,
):
    """`gh run list [--repo …] [--workflow … | --workflow=… ] [--branch …]
    [--commit …] [--limit …] [--json …] [-q …]` → raw CompletedProcess
    (check=False).

    Two workflow spellings are kept distinct: ``workflow=`` emits the split
    ``--workflow <name>`` form (gh-release-issue) while ``workflow_eq=`` emits
    the joined ``--workflow=<name>`` token (release-lex) — each call site's argv
    must stay byte-identical for its BATS stub."""
    args = ["run", "list"]
    if repo is not None:
        args += ["--repo", repo]
    if workflow is not None:
        args += ["--workflow", workflow]
    if workflow_eq is not None:
        args.append(f"--workflow={workflow_eq}")
    if branch is not None:
        args += ["--branch", branch]
    if commit is not None:
        args += ["--commit", commit]
    if limit is not None:
        args += ["--limit", str(limit)]
    if json_fields is not None:
        args += ["--json", ",".join(json_fields)]
    if q is not None:
        args += ["-q", q]
    return _gh_raw(args)


def run_watch(run_id: str, *, repo: str, exit_status: bool = False) -> None:
    """`gh run watch <run_id> --repo … [--exit-status]`.

    Streams the live watch output (no capture) and raises GhError on nonzero —
    mirrors the original `subprocess.run(..., check=True)`."""
    args = ["run", "watch", run_id, "--repo", repo]
    if exit_status:
        args.append("--exit-status")
    _gh_stream(args)


def workflow_run(workflow: str, *, fields: dict[str, str] | None = None):
    """`gh workflow run <workflow> [-f key=value …]` → raw CompletedProcess
    (check=False). Caller forwards gh's stdout/stderr and propagates the exit
    code (workflow_dispatch trigger)."""
    args = ["workflow", "run", workflow]
    for key, value in (fields or {}).items():
        args += ["-f", f"{key}={value}"]
    return _gh_raw(args)


def issue_view(issue: str, *, repo: str, json_fields: list[str]):
    """`gh issue view <issue> --repo … --json …` → raw CompletedProcess
    (check=False). Porcelain (not REST) so the offline BATS stub keeps working;
    caller checks returncode then json.loads(stdout)."""
    return _gh_raw(["issue", "view", issue, "--repo", repo, "--json", ",".join(json_fields)])


def issue_create(*, repo: str, title: str, body: str, label: str | None = None) -> str:
    """`gh issue create --repo … --title … --body … [--label …]` → stripped
    stdout (the new issue URL). Raises GhError on failure (``proc.out``
    semantics)."""
    args = ["issue", "create", "--repo", repo, "--title", title, "--body", body]
    if label is not None:
        args += ["--label", label]
    return _gh(args).strip()


def issue_close(issue: str, *, repo: str, comment: str | None = None):
    """`gh issue close <issue> --repo … [--comment …]` → raw CompletedProcess
    (check=False). Caller maps returncode → success/FAILED."""
    args = ["issue", "close", issue, "--repo", repo]
    if comment is not None:
        args += ["--comment", comment]
    return _gh_raw(args)


def _gh_stream(args: list[str]) -> None:
    """`gh <args>` inheriting the parent's stdout/stderr (no capture) so live
    output (`gh run watch`, `gh pr merge`) streams to the terminal. Raises
    GhError on nonzero — the streaming analogue of `_gh`."""
    if shutil.which("gh") is None:
        raise GhError("`gh` CLI not found on PATH")
    result = proc.run(["gh", *args], capture_output=False, check=False)
    if result.returncode != 0:
        raise GhError(f"gh {' '.join(args)} failed ({result.returncode})")
