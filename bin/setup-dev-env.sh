#!/usr/bin/env bash
# setup-dev-env.sh — Layer 0 bootstrap, installed and managed by `shipit install`.
#
# The managed set owns the consumer ENVIRONMENT (pixi envs, pinned linters,
# issue #547), but everything above rides pixi — and the ADR-0033 pinned
# `bin/shipit` launcher rides uv (`uv tool run` resolves the repo's pin). This
# script provisions that base system: it reconciles pixi and uv TO THEIR PINS
# (reconcile-to-pin, never install-if-missing — a drifted version is reconciled
# exactly like an absent one) from sha256-verified GitHub release tarballs into
# ~/.local/bin, then best-effort pre-solves the repo's pixi environments.
#
# Idempotent and cheap when converged: two version probes plus a locked solve
# that no-ops. LOUD and fail-open on every miss (`setup-dev-env:` warnings on
# stderr, exit 0): it runs from the managed SessionStart hook and must never
# brick a session — a warned, degraded session beats no session. The one
# hard-failing consumer of this script is docker/verify-self-provision.sh,
# which asserts the pins landed.
#
# Release tarballs, never `curl | sh` vendor installers: the Claude Code cloud
# sandbox's default "Trusted" egress allowlist carries github.com and
# release-assets.githubusercontent.com but NOT pixi.sh / astral.sh, so the
# pinned, checksum-verified release asset is the one fetch path that works
# identically on a laptop, in docker, and in a cloud session.
#
# Do not edit — `shipit install` overwrites this file.
set -euo pipefail

# Keep PIXI_PIN in lockstep with `pixi-version` in the wf-checks workflow
# block (.github/workflows/wf-checks.yml, setup-pixi in both its jobs — since
# the TOL01-WS05 cutover ci.yml is a thin caller carrying no pin of its own):
# CI and this bootstrap must provision the same pixi. A drift test
# (tests/test_install.py) pins the two together.
PIXI_PIN="0.71.0"
# uv powers the managed `bin/shipit` launcher's pin resolve (ADR-0033) —
# without it the pinned launcher cannot exec the repo's stamped build.
UV_PIN="0.11.28"

BIN_DIR="${HOME}/.local/bin"

warn() {
	echo "setup-dev-env: $*" >&2
}

# The supported platform triples mirror the fleet pixi platforms (Intel macs
# unsupported, #540): anything else warns and skips — fail-open.
resolve_triple() {
	case "$(uname -s)/$(uname -m)" in
	Linux/x86_64) echo "x86_64-unknown-linux-musl" ;;
	Linux/aarch64) echo "aarch64-unknown-linux-musl" ;;
	Darwin/arm64) echo "aarch64-apple-darwin" ;;
	*) echo "" ;;
	esac
}

sha256_of() {
	# GNU coreutils on Linux, shasum on macOS; "" when neither exists OR the
	# tool errors on the file (#598) — under this script's `set -euo pipefail`
	# an unguarded pipeline would abort the whole run, and "" is what routes a
	# hashing failure into fetch_verified's `[ -z "$got" ]` fail-open path.
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" 2>/dev/null | awk '{print $1}' || echo ""
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" 2>/dev/null | awk '{print $1}' || echo ""
	else
		echo ""
	fi
}

probe_version() {
	# The first X.Y.Z token of `<tool> --version`, or "" (tool absent included).
	"$1" --version 2>/dev/null | head -n 1 | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1 || true
}

place_binary() {
	# $1 = source file, $2 = dest basename. Atomic within BIN_DIR: staged copy
	# + same-dir `mv -f`, so a concurrent invocation never sees a torn binary.
	local staged
	staged="${BIN_DIR}/.${2}.setup-dev-env.$$"
	cp "$1" "$staged" && chmod +x "$staged" && mv -f "$staged" "${BIN_DIR}/${2}"
}

fetch_verified() {
	# $1 = URL, $2 = expected sha256, $3 = dest file. The checksum is pinned in
	# this script (verified from the published release assets), so a tampered or
	# truncated download can never be installed.
	local got
	if ! command -v curl >/dev/null 2>&1; then
		warn "curl is not available — cannot fetch $1"
		return 1
	fi
	if ! curl -fsSL --retry 2 -o "$3" "$1"; then
		warn "could not fetch $1"
		return 1
	fi
	got="$(sha256_of "$3")"
	if [ -z "$got" ]; then
		warn "could not hash $3 (no sha256sum/shasum, or the tool errored) — refusing the unverified $1"
		return 1
	fi
	if [ "$got" != "$2" ]; then
		warn "sha256 mismatch for $1 (got ${got}, want $2) — refusing to install"
		return 1
	fi
}

provision_pixi() {
	local url sum tmp
	url="https://github.com/prefix-dev/pixi/releases/download/v${PIXI_PIN}/pixi-${TRIPLE}.tar.gz"
	case "$TRIPLE" in
	x86_64-unknown-linux-musl) sum="2f30a2434b3786c860d11494f4dc6c1f3437fb47366d948e398409cae84e0a6c" ;;
	aarch64-unknown-linux-musl) sum="568696c74bd734becf8c7bb84b7d5ea9beda58031f66a6288a8dbc47131dfbf9" ;;
	aarch64-apple-darwin) sum="b3c7e0470a89f63db5b962a72141813e643752825ee8fd950f169ddb4a3d2a44" ;;
	*) return 1 ;;
	esac
	tmp="$(mktemp -d)" || return 1
	if ! fetch_verified "$url" "$sum" "${tmp}/pixi.tar.gz" ||
		! tar -xzf "${tmp}/pixi.tar.gz" -C "$tmp" pixi ||
		! place_binary "${tmp}/pixi" pixi; then
		rm -rf "$tmp"
		return 1
	fi
	rm -rf "$tmp"
}

provision_uv() {
	# The uv tarball nests its binaries under uv-<triple>/; install uv AND uvx
	# (both ship in the asset, and uvx is the sibling entry point).
	local url sum tmp
	url="https://github.com/astral-sh/uv/releases/download/${UV_PIN}/uv-${TRIPLE}.tar.gz"
	case "$TRIPLE" in
	x86_64-unknown-linux-musl) sum="f02146b371c35c287d860f003ece7345c86e358a3fd70a9b63700cd141ee7fb4" ;;
	aarch64-unknown-linux-musl) sum="da10cdfa7d92212b7acb62021a0fd61bcf8580c58c3632ec915d10c3a1a7906b" ;;
	aarch64-apple-darwin) sum="33540eb7c883ab857eff79bd5ac2aa31fe27b595abecb4a9c003a2c998447232" ;;
	*) return 1 ;;
	esac
	tmp="$(mktemp -d)" || return 1
	if ! fetch_verified "$url" "$sum" "${tmp}/uv.tar.gz" ||
		! tar -xzf "${tmp}/uv.tar.gz" -C "$tmp" ||
		! place_binary "${tmp}/uv-${TRIPLE}/uv" uv ||
		! place_binary "${tmp}/uv-${TRIPLE}/uvx" uvx; then
		rm -rf "$tmp"
		return 1
	fi
	rm -rf "$tmp"
}

provision_tool() {
	# Direct dispatch (no `"$fn"` indirection — shellcheck 0.10's reachability
	# analysis cannot follow an indirect call and would flag the provisioners
	# as unreachable, SC2317).
	case "$1" in
	pixi) provision_pixi ;;
	uv) provision_uv ;;
	*) return 1 ;;
	esac
}

reconcile_tool() {
	# $1 = tool, $2 = pin. Exact pin match → no-op; anything else (absent OR
	# drifted) → fetch + install, then re-probe and warn LOUDLY when the
	# resolved version still mismatches (a PATH shadow is hiding the pinned
	# binary). Always returns 0 — fail-open.
	local have
	have="$(probe_version "$1")"
	if [ "$have" = "$2" ]; then
		return 0
	fi
	if [ -z "$TRIPLE" ]; then
		warn "unsupported platform $(uname -s)/$(uname -m) — cannot provision $1 $2 (found: ${have:-none})"
		return 0
	fi
	warn "reconciling $1 to $2 (found: ${have:-none})"
	if ! provision_tool "$1"; then
		warn "$1 $2 was NOT provisioned — later steps that need it will degrade"
		return 0
	fi
	have="$(probe_version "$1")"
	if [ "$have" != "$2" ]; then
		warn "installed $1 $2 into ${BIN_DIR}, but '$1 --version' resolves ${have:-nothing} — a PATH entry is shadowing the pinned binary (check 'command -v $1')"
	fi
}

manifest_defines_lint_env() {
	# Does pixi.toml's [environments] table define a `lint` env (the managed env
	# block)? Table-scoped awk read, same pattern as the bin/shipit launcher's
	# pin read — a flat grep would false-positive on the managed `[tasks]`
	# `lint = "./bin/shipit lint"` line.
	awk '
		{ gsub(/\r/, "") }
		/^\[/ { in_envs = ($0 == "[environments]") ? 1 : 0; next }
		in_envs && $0 ~ /^[[:space:]]*lint[[:space:]]*=/ { found = 1; exit }
		END { exit !found }
	' "${REPO_ROOT}/pixi.toml"
}

TRIPLE="$(resolve_triple)"
SELF="${BASH_SOURCE[0]:-$0}"
REPO_ROOT="$(cd "$(dirname "$SELF")/.." && pwd)"

if ! mkdir -p "$BIN_DIR"; then
	warn "could not create ${BIN_DIR} — nothing can be provisioned"
	exit 0
fi

# ~/.local/bin must LEAD PATH for this run so the freshly placed pins win the
# re-probe (and the pixi solve below) over any stale system copy.
PATH="${BIN_DIR}:${PATH}"
export PATH

# When Claude Code hands us a session env file (cloud + SES01 sessions),
# idempotently append a guarded PATH line so every LATER Bash call in the
# session resolves the pins too — the marker comment keys the idempotence.
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
	if ! grep -Fqs "setup-dev-env: pinned base-system PATH" "$CLAUDE_ENV_FILE"; then
		if ! printf '%s\n' "case \":\$PATH:\" in *\":\$HOME/.local/bin:\"*) ;; *) export PATH=\"\$HOME/.local/bin:\$PATH\" ;; esac # setup-dev-env: pinned base-system PATH" >>"$CLAUDE_ENV_FILE"; then
			warn "could not append the PATH line to CLAUDE_ENV_FILE (${CLAUDE_ENV_FILE})"
		fi
	fi
fi

reconcile_tool pixi "$PIXI_PIN"
reconcile_tool uv "$UV_PIN"

# Best-effort environment pre-solve. `--locked` ONLY: provisioning must never
# mutate pixi.lock (ADR-0033 — provisioning mutates nothing managed); a lock
# drift fails the solve loudly here and stays the repo's own problem. Fast
# no-op when the envs are already solved against the lockfile.
if [ -f "${REPO_ROOT}/pixi.toml" ]; then
	if ! command -v pixi >/dev/null 2>&1; then
		warn "pixi is unavailable — skipping the environment solve"
	else
		if ! (cd "$REPO_ROOT" && pixi install --locked); then
			warn "pixi install --locked failed (default env) — the next pixi run will surface the error"
		fi
		if manifest_defines_lint_env; then
			if ! (cd "$REPO_ROOT" && pixi install --locked --environment lint); then
				warn "pixi install --locked -e lint failed — the next lint run will surface the error"
			fi
		fi
	fi
fi

exit 0
