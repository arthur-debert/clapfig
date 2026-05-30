"""The single boundary to GitHub: shell out to `gh`, parse JSON with the stdlib.

Why `gh` rather than a Python client: `gh` is already provisioned in every
environment the engine runs in (local, Cloud), handles auth + pagination, and
— crucially — speaks GraphQL, where the PR review-thread and resolution data
live. Keeping the boundary here means the rest of the package is pure data
transformation and unit-tests without the network.
"""

from __future__ import annotations

import json
import shutil
import subprocess


class GhError(RuntimeError):
    """A `gh` invocation failed, or `gh` is unavailable."""


def _gh(args: list[str], *, input_text: str | None = None) -> str:
    if shutil.which("gh") is None:
        raise GhError("`gh` CLI not found on PATH")
    proc = subprocess.run(  # noqa: S603 — args are constructed, never shell-interpolated
        ["gh", *args],
        capture_output=True,
        text=True,
        input=input_text,
        check=False,
    )
    if proc.returncode != 0:
        raise GhError(f"gh {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}")
    return proc.stdout


def rest(
    path: str,
    *,
    paginate: bool = False,
    method: str | None = None,
    fields: dict[str, str] | None = None,
) -> object:
    """Call `gh api <path>` and return parsed JSON (None on empty output)."""
    args = ["api"]
    if method:
        args += ["-X", method]
    if paginate:
        args.append("--paginate")
    for key, value in (fields or {}).items():
        args += ["-f", f"{key}={value}"]
    args.append(path)
    out = _gh(args)
    if not out.strip():
        return None
    if paginate:
        return _merge_paginated(out)
    return json.loads(out)


def _merge_paginated(out: str) -> list:
    """`gh api --paginate` concatenates one JSON array per page; flatten them."""
    merged: list = []
    decoder = json.JSONDecoder()
    text = out.strip()
    idx = 0
    while idx < len(text):
        obj, end = decoder.raw_decode(text, idx)
        merged.extend(obj if isinstance(obj, list) else [obj])
        idx = end
        while idx < len(text) and text[idx] in " \n\r\t":
            idx += 1
    return merged


def graphql(query: str, **variables: object) -> dict:
    """Run a GraphQL query/mutation; return the `data` object, raising on errors."""
    args = ["api", "graphql", "-f", f"query={query}"]
    for key, value in variables.items():
        # -F type-infers ints/bools; -f forces a string (needed for ID! vars).
        flag = "-F" if isinstance(value, (int, bool)) else "-f"
        args += [flag, f"{key}={value}"]
    payload = json.loads(_gh(args))
    if payload.get("errors"):
        raise GhError(f"graphql errors: {payload['errors']}")
    return payload["data"]


def repo_slug() -> tuple[str, str]:
    """Return (owner, name) for the current repo."""
    data = json.loads(_gh(["repo", "view", "--json", "owner,name"]))
    return data["owner"]["login"], data["name"]


def pr_meta(pr: int) -> dict:
    """PR-level metadata the engine needs in one call."""
    out = _gh(
        [
            "pr",
            "view",
            str(pr),
            "--json",
            "number,headRefOid,baseRefName,isDraft,mergeable,mergeStateStatus,"
            "reviewRequests,statusCheckRollup",
        ]
    )
    return json.loads(out)
