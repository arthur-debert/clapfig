"""yamlio — YAML read, the one thing the stdlib can't do.

Phase 0–1 shells out to `yq` (already a required external CLI in release) and
parses its JSON output with the stdlib. This is the single seam: if/when we
adopt PyYAML (the deferred dependency decision — proposal §dependency
frontier), only this module changes.
"""

from __future__ import annotations

import json
import shutil

from . import proc


class YamlError(RuntimeError):
    """`yq` is unavailable or failed to parse the document."""


def _yq(args: list[str], *, input_text: str | None = None) -> str:
    if shutil.which("yq") is None:
        raise YamlError("`yq` CLI not found on PATH")
    result = proc.run(["yq", *args], input=input_text, check=False)
    if result.returncode != 0:
        raise YamlError(
            f"yq {' '.join(args)} failed ({result.returncode}): {result.stderr.strip()}"
        )
    return result.stdout


def load(path: str) -> object:
    """Parse the YAML file at ``path`` → Python object (via ``yq -o=json``)."""
    out = _yq(["-o=json", ".", path])
    if not out.strip():
        return None
    return json.loads(out)


def loads(text: str) -> object:
    """Parse a YAML string → Python object (via ``yq -o=json`` over stdin)."""
    out = _yq(["-o=json", "."], input_text=text)
    if not out.strip():
        return None
    return json.loads(out)


def eval_all(expr: str, files: list[str]) -> str:
    """`yq eval-all '<expr>' <files...>` → raw YAML stdout (NOT JSON).

    Added for release-sync's lefthook.yml composition (Phase 2): the bash piped
    several fragment files through ``yq eval-all '. as $i ireduce({}; . *+ $i) |
    ... comments=""'`` to deep-merge them in order and strip comments. This is
    the YAML→YAML transform seam; keep it here so the yq boundary stays single."""
    return _yq(["eval-all", expr, *files])
