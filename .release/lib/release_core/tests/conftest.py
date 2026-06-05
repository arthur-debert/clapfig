"""Shared fixture loader for the prstate (PR state engine) tests.

Each JSON file under prstate_fixtures/ holds the raw `gh` payloads for one PR
scenario; `context` builds a PullContext from one exactly as `fetch.gather()`
would, minus the network. These are hand-shaped now and replaced with real
captured responses by the Live-verification phase (issue #337).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from release_core.prstate.fetch import context_from_raw
from release_core.prstate.model import PullContext

FIXTURES = Path(__file__).parent / "prstate_fixtures"


def load_context(name: str) -> PullContext:
    data = json.loads((FIXTURES / f"{name}.json").read_text())
    return context_from_raw(
        meta=data["meta"],
        reviews_json=data.get("reviews", []),
        thread_nodes=data.get("threads", []),
        reactions=data.get("reactions", []),
        issue_comments=data.get("issue_comments", []),
        review_comments=data.get("review_comments", []),
    )


@pytest.fixture
def context():
    """Return the loader so a test can pick its scenario: `context('name')`."""
    return load_context
