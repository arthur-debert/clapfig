"""yamlio — YAML read via `yq`. Exercises the real `yq` (the production seam);
skips where `yq` is unavailable rather than mocking subprocess (per contract)."""

from __future__ import annotations

import shutil

import pytest
from release_core import yamlio

yq_required = pytest.mark.skipif(shutil.which("yq") is None, reason="`yq` not on PATH")


@yq_required
def test_loads_mapping():
    assert yamlio.loads("a: 1\nb: two\n") == {"a": 1, "b": "two"}


@yq_required
def test_loads_list():
    assert yamlio.loads("- x\n- y\n") == ["x", "y"]


@yq_required
def test_loads_nested():
    data = yamlio.loads("capabilities:\n  - mkdocs\n  - bats\n")
    assert data == {"capabilities": ["mkdocs", "bats"]}


@yq_required
def test_load_file(tmp_path):
    p = tmp_path / "c.yaml"
    p.write_text("k: v\n")
    assert yamlio.load(str(p)) == {"k": "v"}


def test_missing_yq_raises(monkeypatch):
    monkeypatch.setattr(yamlio.shutil, "which", lambda _: None)
    with pytest.raises(yamlio.YamlError):
        yamlio.loads("a: 1")
