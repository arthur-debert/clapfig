#!/usr/bin/env bash
# tombi-proof.sh — the consumer proof for the editor schema directive (#103).
#
# Generates an edward-shaped artifact pair (`blocks.toml` + its JSON Schema)
# through the `schema_directive` example, then runs tombi — the TOML editor
# toolchain the directive exists for — against the generated template. Five
# checks, in order:
#
#   1. The generated template lints clean against the schema its own
#      `#:schema` line names.
#   2. A real block instance appended to it still lints clean, so the schema
#      describes documents users actually write, not just an empty file. It
#      includes the multiword `load-order` key: the example generates with
#      `normalize_keys(true)`, so tombi checks that the schema declares the
#      kebab spelling the template writes.
#   3. The same instance spelled `load_order` ALSO lints clean. That builder
#      loads either spelling, so a schema accepting only the generated one
#      would flag a config file that clapfig reads without complaint.
#   4. An instance holding BOTH spellings of that one key is REJECTED, the
#      way the load path refuses it rather than picking a winner by key
#      order.
#   5. An unknown key is REJECTED. This is the control: without it, a tombi
#      that silently ignored the directive would pass every check above.
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

# Lint the generated template with $2 appended to it, expecting tombi to
# accept ($1 = accepts) or reject ($1 = rejects) the result. Each case
# starts from the pristine template, so one case cannot mask another.
check() {
	local expectation="$1" title="$2" body="$3"
	cat template.toml >blocks.toml
	printf '%s' "$body" >>blocks.toml
	echo "tombi-proof: [$title] $expectation"
	if tombi lint blocks.toml; then
		[[ $expectation == accepts ]] && return 0
		echo "tombi-proof: FAILED — tombi accepted a document the schema forbids." >&2
		exit 1
	fi
	[[ $expectation == rejects ]] && {
		echo "             (the tombi error above is the expected outcome)"
		return 0
	}
	echo "tombi-proof: FAILED — tombi rejected a document clapfig loads." >&2
	exit 1
}

echo "tombi-proof: generating the artifact pair"
(cd "$repo_root" && cargo run --quiet --example schema_directive -- "$workdir")
cd "$workdir"
cp blocks.toml template.toml

echo "tombi-proof: generated template"
sed 's/^/  | /' template.toml

check accepts "1/5 the generated template" ""

check accepts "2/5 a filled-in instance, kebab spelling" '
[block.core]
kind = "rust"
mount = "crates/core"
load-order = 10
'

check accepts "3/5 the same instance, declared snake_case spelling" '
[block.core]
kind = "rust"
mount = "crates/core"
load_order = 10
'

check rejects "4/5 both spellings of one key in one table" '
[block.core]
kind = "rust"
load-order = 10
load_order = 10
'

check rejects "5/5 an unknown key (the control: the directive is live)" '
[block.core]
kind = "rust"
bogus_key = 1
'

echo "tombi-proof: OK"
