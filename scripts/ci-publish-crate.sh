#!/usr/bin/env bash
# Publish one workspace crate to crates.io, tolerating "already uploaded".
#
# Usage: scripts/ci-publish-crate.sh <crate-name>
#
# `cargo publish` (1.66+) already waits for the crate to be available in the
# sparse index before returning, so no extra polling is needed here. The
# pre-check against crates.io's JSON API is only to skip the whole compile
# step when this version is already published (e.g. re-running a failed job).
#
# Robustness vs prior fragile patterns: tolerates "already exists on
# crates.io index" / "already uploaded" cargo errors so re-runs of a
# release after a partial publish succeed instead of aborting at the
# first already-published crate.

set -euo pipefail

crate="${1:?crate name required}"

version=$(
    cargo metadata --format-version 1 --no-deps \
        | python3 -c "
import json, sys
for p in json.load(sys.stdin)['packages']:
    if p['name'] == '$crate':
        print(p['version'])
        break
"
)

if [[ -z "$version" ]]; then
    echo "✗ could not resolve version for crate '$crate'" >&2
    exit 1
fi

if curl -sf -o /dev/null "https://crates.io/api/v1/crates/$crate/$version"; then
    echo "✓ $crate $version already on crates.io — skipping publish"
    exit 0
fi

echo "→ publishing $crate $version"
log=$(mktemp)
if cargo publish -p "$crate" 2>&1 | tee "$log"; then
    echo "✓ $crate $version published"
elif grep -qE "already uploaded|is already uploaded|already exists on crates\.io index" "$log"; then
    echo "✓ $crate $version already uploaded (race) — continuing"
else
    echo "✗ $crate $version publish failed" >&2
    exit 1
fi
