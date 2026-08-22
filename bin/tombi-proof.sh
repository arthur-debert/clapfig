#!/usr/bin/env bash
# tombi-proof.sh — the consumer proof for the editor schema directive (#103).
#
# Generates an edward-shaped artifact pair (`blocks.toml` + its JSON Schema)
# through the `schema_directive` example, then runs tombi — the TOML editor
# toolchain the directive exists for — against the generated template. Three
# checks, in order:
#
#   1. The generated template lints clean against the schema its own
#      `#:schema` line names.
#   2. A real block instance appended to it still lints clean, so the schema
#      describes documents users actually write, not just an empty file. It
#      includes the multiword `load-order` key: the example generates with
#      `normalize_keys(true)`, so tombi checks that the schema declares the
#      kebab spelling the template writes rather than the field's
#      `load_order` name.
#   3. An unknown key is REJECTED. This is the control: without it, a tombi
#      that silently ignored the directive would pass checks 1 and 2.
#
# tombi is not a repo dependency — it runs from a pinned uvx-resolved release,
# so this reproduces on any machine with uv (which `pixi run` already
# provides) and does not depend on a global install.
#
# Usage: bin/tombi-proof.sh

set -euo pipefail

TOMBI_PIN="tombi==1.4.1"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

tombi() {
	uvx --from "$TOMBI_PIN" tombi "$@"
}

echo "tombi-proof: generating the artifact pair"
(cd "$repo_root" && cargo run --quiet --example schema_directive -- "$workdir")
cd "$workdir"

echo "tombi-proof: generated template"
sed 's/^/  | /' blocks.toml

echo "tombi-proof: [1/3] the generated template validates against its own schema"
tombi lint blocks.toml

echo "tombi-proof: [2/3] a filled-in block instance validates"
cat >>blocks.toml <<'EOF'

[block.core]
kind = "rust"
mount = "crates/core"
load-order = 10
EOF
tombi lint blocks.toml

echo "tombi-proof: [3/3] an unknown key must be rejected (the directive is live);"
echo "             the tombi error printed below is the expected outcome"
echo 'bogus_key = 1' >>blocks.toml
if tombi lint blocks.toml; then
	echo "tombi-proof: FAILED — tombi accepted a document the schema forbids;" >&2
	echo "             the #:schema directive is not being resolved." >&2
	exit 1
fi

echo "tombi-proof: OK"
