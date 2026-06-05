"""Pure-logic tests for the gh boundary (no subprocess)."""

from __future__ import annotations

from release_core.prstate.ghapi import _merge_paginated


def test_merge_paginated_flattens_concatenated_arrays():
    # `gh api --paginate` emits one JSON array per page, concatenated.
    out = '[{"id": 1}, {"id": 2}]\n[{"id": 3}]\n'
    assert [o["id"] for o in _merge_paginated(out)] == [1, 2, 3]


def test_merge_paginated_single_page():
    assert _merge_paginated('[{"id": 1}]') == [{"id": 1}]
