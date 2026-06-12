#!/usr/bin/env bash
# bin/setup-dev-env.sh — per-session dev-environment setup, invoked by
# the SessionStart hook in .claude/settings.json.
#
# Source of truth: arthur-debert/release templates/commons/bin/setup-dev-env.sh.
# Synced to consumers by release-sync (full file replace, no markers).
#
# Repos that need project-specific extras (Xvfb daemon, pinned-binary
# fetch, extra rustup targets, etc.) put them in app-bin/post-setup-hook.sh
# — this script calls that hook at the end if it exists.
#
# Pre-commit hook wiring runs in BOTH local and cloud sessions (a fresh
# clone has no `.git/hooks/pre-commit` wired regardless of where the dev
# is). Everything else below the cloud-only gate is cloud-only —
# submodules, project dep caches, NSS cert imports etc. are already in
# place on a dev's local machine.
#
# Detects stack by filesystem signals — handles rust, node, ruby, python,
# and consumers with no project deps (just lefthook / hand-rolled hook
# wiring).
#
# Idempotent — safe to re-run. Errors are best-effort: a failure in one
# step does not abort the rest (transient registry hiccups shouldn't
# block the lefthook install).

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "${REPO_ROOT}"

# --- 0. Arm the gate: ensure the lefthook toolset is installed ----------
# THE gate (lefthook.yml) is HARD — a missing tool FAILS the commit, it does
# not skip. So the bootstrap must guarantee the toolset in every environment
# (the gate runs at session start, local pre-commit, and CI alike). Runs ABOVE
# the cloud-only gate because the lint gate runs everywhere, not just in cloud.
# Idempotent: each tool installs only when absent (no-op on a set-up machine);
# a loud warning (never silent) surfaces an unarmed gate HERE rather than at
# commit time. Best-effort installs — a transient failure shouldn't abort setup.

_warn_unarmed() {
  echo "warning: '$1' not installed and could not be auto-installed — the lefthook gate will FAIL until it is present. Install it, then re-run." >&2
}

# Pinned gate-toolset versions + the gate_version_matches reconcile helper —
# single source of truth, shared with the CI provisioner
# bin-internal/provision-gate-toolset.sh so the two can't drift (release#498,
# release#531). Synced alongside this script (same managed sync) so it is
# normally always present; the `:=` fallbacks + the gate_version_matches fallback
# are a last-resort safety net so a (broken) missing-file state can't abort the
# SessionStart hook before install-release-core can self-heal. The `:=` fallbacks
# are kept matching the shared file by tests/gate-tool-versions/.
# shellcheck source=/dev/null
[ -f "${REPO_ROOT}/bin/gate-tool-versions.sh" ] && . "${REPO_ROOT}/bin/gate-tool-versions.sh"
: "${RUFF_VERSION:=0.15.12}"
: "${ACTIONLINT_VERSION:=1.7.7}"
: "${YAMLLINT_VERSION:=1.38.0}"
: "${LEFTHOOK_VERSION:=2.1.9}"
: "${PRETTIER_VERSION:=3.8.4}"
: "${MARKDOWNLINT_CLI_VERSION:=0.48.0}"
: "${SHELLCHECK_VERSION:=0.11.0}"
: "${SHELLCHECK_PY_VERSION:=0.11.0.1}"
command -v gate_version_matches >/dev/null 2>&1 || gate_version_matches() {
  command -v "$1" >/dev/null 2>&1 || return 1
  _gvm_have="$("$1" --version 2>/dev/null \
    | grep -Eo '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1)"
  [ "$_gvm_have" = "$2" ]
}

# RECONCILE every tool to its pin (release#531), NOT install-if-missing: a
# pre-existing floating brew/apt/npm binary reports a different version and gets
# reinstalled at the pin instead of silently winning — the bug that let a dev box
# run actionlint 1.7.12 while CI ran 1.7.7, so the same gate gave different
# verdicts. Best-effort installs (a transient failure shouldn't abort the hook);
# a loud re-check warning surfaces any still-unpinned tool HERE, not at commit.

# npm globals: lefthook + prettier + markdownlint, each at its pin. Installed
# before the hook wiring below so `lefthook install` finds the binary. The
# markdownlint-cli package's binary is `markdownlint`.
npm_pkgs=()
gate_version_matches lefthook     "$LEFTHOOK_VERSION"          || npm_pkgs+=("lefthook@${LEFTHOOK_VERSION}")
gate_version_matches prettier     "$PRETTIER_VERSION"          || npm_pkgs+=("prettier@${PRETTIER_VERSION}")
gate_version_matches markdownlint "$MARKDOWNLINT_CLI_VERSION"  || npm_pkgs+=("markdownlint-cli@${MARKDOWNLINT_CLI_VERSION}")
if [ "${#npm_pkgs[@]}" -gt 0 ] && command -v npm >/dev/null 2>&1; then
  npm install -g "${npm_pkgs[@]}" >/dev/null 2>&1 || true
fi
gate_version_matches lefthook     "$LEFTHOOK_VERSION"          || _warn_unarmed "lefthook@${LEFTHOOK_VERSION}"
gate_version_matches prettier     "$PRETTIER_VERSION"          || _warn_unarmed "prettier@${PRETTIER_VERSION}"
gate_version_matches markdownlint "$MARKDOWNLINT_CLI_VERSION"  || _warn_unarmed "markdownlint@${MARKDOWNLINT_CLI_VERSION}"

# pip tools: ruff + yamllint + shellcheck (via the shellcheck-py wheel — bundles
# the binary, so shellcheck is pinned identically to ruff/yamllint with no
# apt-0.9.0-vs-brew-0.11 split). Pinned to the CI versions so local and CI never
# disagree on findings. Modern Debian/Ubuntu (PEP 668) reject a global pip
# install without --break-system-packages; try that first, fall back to a plain
# install (older distros / venvs reject the flag). Install stderr stays visible.
if ! gate_version_matches ruff "$RUFF_VERSION" \
  || ! gate_version_matches yamllint "$YAMLLINT_VERSION" \
  || ! gate_version_matches shellcheck "$SHELLCHECK_VERSION"; then
  if command -v pip3 >/dev/null 2>&1; then
    pip3 install --quiet --break-system-packages \
      "ruff==${RUFF_VERSION}" "yamllint==${YAMLLINT_VERSION}" "shellcheck-py==${SHELLCHECK_PY_VERSION}" >/dev/null \
      || pip3 install --quiet \
      "ruff==${RUFF_VERSION}" "yamllint==${YAMLLINT_VERSION}" "shellcheck-py==${SHELLCHECK_PY_VERSION}" >/dev/null || true
  fi
fi
gate_version_matches ruff       "$RUFF_VERSION"       || _warn_unarmed "ruff==${RUFF_VERSION}"
gate_version_matches yamllint   "$YAMLLINT_VERSION"   || _warn_unarmed "yamllint==${YAMLLINT_VERSION}"
gate_version_matches shellcheck "$SHELLCHECK_VERSION" || _warn_unarmed "shellcheck==${SHELLCHECK_VERSION}"

# actionlint: pinned official downloader → ~/.local/bin. apt has no actionlint
# package and brew floats, so the downloader is the one cross-OS pinned source.
# ~/.local/bin (not /usr/local/bin) needs no sudo and is first on PATH, so the
# pinned binary deterministically shadows any floating brew/apt actionlint — the
# install-if-missing hole that let a brew-installed actionlint outvote the pin.
if ! gate_version_matches actionlint "$ACTIONLINT_VERSION" && command -v curl >/dev/null 2>&1; then
  mkdir -p "${HOME}/.local/bin"
  _actionlint_url="https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash"
  curl -sSfL "${_actionlint_url}" \
    | bash -s -- "${ACTIONLINT_VERSION}" "${HOME}/.local/bin" >/dev/null || true
fi
gate_version_matches actionlint "$ACTIONLINT_VERSION" || _warn_unarmed "actionlint==${ACTIONLINT_VERSION}"

# golangci-lint — the go-quality gate's linter. Only Go repos run that hook, so
# gate the install on a root go.mod existing (we cd'd to REPO_ROOT above; the
# §2 `go mod download` block uses the same root check). brew on macOS; on Linux
# the canonical install script drops the binary into the Go bin dir, with `go
# install` as the fallback when go is present but curl/sh isn't. Pinned to a
# version (single source) for reproducibility. Install stderr stays visible —
# matches the ruff/yamllint blocks; only stdout is muted.
_GOLANGCI_LINT_VERSION="v1.64.8"
if [ -f go.mod ] && ! command -v golangci-lint >/dev/null 2>&1; then
  if command -v brew >/dev/null 2>&1; then
    brew install golangci-lint >/dev/null || true
  elif command -v go >/dev/null 2>&1; then
    # GOBIN wins if set; else the FIRST entry of a (possibly colon-separated)
    # GOPATH — `$(go env GOPATH)/bin` would be a broken path on a multi-entry
    # GOPATH. The install script and `go install` both land the binary here.
    _go_bin="$(go env GOBIN)"
    [ -n "${_go_bin}" ] || _go_bin="$(go env GOPATH | cut -d: -f1)/bin"
    if command -v curl >/dev/null 2>&1; then
      curl -sSfL https://raw.githubusercontent.com/golangci/golangci-lint/master/install.sh \
        | sh -s -- -b "${_go_bin}" "${_GOLANGCI_LINT_VERSION}" >/dev/null \
        || go install "github.com/golangci/golangci-lint/cmd/golangci-lint@${_GOLANGCI_LINT_VERSION}" >/dev/null \
        || true
    else
      go install "github.com/golangci/golangci-lint/cmd/golangci-lint@${_GOLANGCI_LINT_VERSION}" >/dev/null || true
    fi
  fi
  command -v golangci-lint >/dev/null 2>&1 || _warn_unarmed golangci-lint
fi

# --- 0.1. Pull-model: self-update release_core from the published wheel --
# The north star (ADR-0003): repos AUTO-UPDATE on session start — this REPLACED
# the hand-run push treadmill (`orc propagate`, since removed). The boot resolver
# `install-release-core`
# resolves the latest release wheel, pip-installs it (--force-reinstall — the
# wheel version is static, so `-U` would skip it; deps resolve from PyPI), THEN runs `release-core
# init` itself (it locates the just-installed console-script across venv/--user/
# system layouts). A bare `init` now materializes the WHOLE managed tree from the
# wheel bundle (the .release/ build dir + every working-tree mirror — skills,
# ORIENTATION, configs, the CLAUDE.md block) and auto-commits any managed change
# (#476 cutover) — not just the config subset. So SessionStart self-syncs the full
# tree from the pulled wheel: no push needed (no push mechanism exists). One
# command does the whole boot. Runs in BOTH local and cloud (above the cloud-only
# gate) — auto-update is the whole point. Runs BEFORE the hook wiring (release#567):
# init materializes the ephemeral .release/ (the gate config), so the very first
# session of a fresh clone wires a hook that has a gate to run.
#
# BEST-EFFORT, never aborts the session: every call is `|| warn`, and init
# failure inside the resolver is itself best-effort. The committed tree already
# in the repo degrades gracefully if the pull fails (a stale repo is
# older-but-working, never broken).
#
# The resolver is part of the committed bootstrap: it ships in the synced set
# (templates/commons/bin/ → consumer bin/install-release-core), so it is found at
# `$REPO_ROOT/bin/install-release-core` without needing the consumer's bin/ on
# PATH. Fall back to a PATH lookup (release-dev, where dodot puts bin/ on PATH).
# If neither resolves (a consumer not yet seeded with the resolver), the block
# no-ops — safe to land fleet-wide before the seeding propagate.
#
# Install target: the resolver self-isolates — it installs release_core into its
# OWN dedicated venv (never the user pip / system site / a project venv) and
# symlinks the console-scripts onto PATH (~/.local/bin). So there's nothing to
# choose here: just invoke it. (It still tolerates a stale caller passing
# --user/--break-system-packages, so an in-flight fleet migration can't break.)
_resolver=""
if [ -x "${REPO_ROOT}/bin/install-release-core" ]; then
  _resolver="${REPO_ROOT}/bin/install-release-core"
elif command -v install-release-core >/dev/null 2>&1; then
  _resolver="install-release-core"
fi
if [ -n "${_resolver}" ]; then
  "${_resolver}" \
    || echo "warning: install-release-core failed — release_core not updated this session" >&2
fi

# --- 0.2. Pre-commit hook wiring (BOTH local and cloud) -----------------
# Wiring `.git/hooks/pre-commit` is per-clone state — every fresh clone
# (and cloud snapshot) starts without it, so we wire it on every session
# and in both contexts. Runs ABOVE the cloud-only gate because skipping
# it locally is what produces the "agents are still running husky"
# symptom: lefthook never gets installed, husky's old wiring keeps
# firing.
#
# Runs AFTER the pull-model boot (§0.1) deliberately (release#567): on a
# fresh clone/session the boot installs release-core and `release-core
# init` materializes the ephemeral `.release/` (gitignored since WS4, so
# EVERY session starts without it) — wiring first left the earliest
# commits of a session without a hook at all, and a previously-wired hook
# firing before init warn-and-passed on the missing config (the
# first-commit-of-session boot hole; gate now fails loud on that too).
#
# Default: lefthook (binary installed at env-setup time in cloud, by
# brew/cargo/npm locally). Fallback for repos that ship a hand-rolled
# app-bin/pre-commit instead (zed-lex, tree-sitter-lex pattern): symlink
# it into .git/hooks/.
#
# Husky migration: if a previous `husky install` set
# `core.hooksPath=.husky`, git routes hooks to `.husky/pre-commit` and
# ignores `.git/hooks/pre-commit` entirely — so `lefthook install`
# silently no-ops. Clear that config first when migrating a repo to
# lefthook. We do NOT delete `.husky/` itself; that's a consumer-side
# cleanup (committed file, belongs in a PR).

# Resolve lefthook binary. npm/pnpm consumers commonly have lefthook
# installed at `node_modules/.bin/lefthook` (via `prepare: lefthook install`
# in package.json) — `command -v lefthook` doesn't find that location, so
# without this check the script silently falls through to the
# app-bin/pre-commit branch in cloud sessions for npm consumers.
_lefthook=""
if [ -x node_modules/.bin/lefthook ]; then
  _lefthook="node_modules/.bin/lefthook"
elif command -v lefthook >/dev/null 2>&1; then
  _lefthook="lefthook"
fi

if command -v release-core >/dev/null 2>&1 \
   && { [ -f .release/lefthook.yml ] || [ ! -f lefthook.yml ]; }; then
  # WS3 (release#524): the gate definition lives ONLY in the ephemeral .release/
  # build dir — there is no tracked root lefthook.yml for a stock `lefthook
  # install` shim to discover. Wire the git hook through the binary instead:
  # `release-core gate --install-hook` writes .git/hooks/pre-commit (→ `release-core
  # gate --hook`, which points lefthook at .release/lefthook.yml) and unsets any
  # stale core.hooksPath redirect itself. This is the path every migrated consumer
  # takes; the root-lefthook.yml branch below is for release's own repo (which
  # keeps a hand-authored root gate) and not-yet-migrated consumers.
  #
  # The no-root-lefthook.yml arm (release#567): a consumer whose §0.1 init failed
  # (network hiccup) has no .release/lefthook.yml YET — wire the binary hook
  # anyway, so its commits hit `release-core gate --hook`'s fail-loud
  # unmaterialized-config error instead of running ungated with no hook at all.
  if ! release-core gate --install-hook >/dev/null; then
    echo "warning: release-core gate --install-hook failed — pre-commit hook NOT wired" >&2
  fi
elif [ -f lefthook.yml ] && [ -n "${_lefthook}" ]; then
  # `git config --get` returns 1 when unset. Command substitution exit
  # codes don't propagate `set -e` from a conditional context, but the
  # explicit `|| true` makes the empty-when-unset intent unambiguous.
  _hooks_path="$(git config --get core.hooksPath 2>/dev/null || true)"
  # Unset core.hooksPath if set to ANY value (not just .husky). Any
  # custom hooksPath redirects git away from .git/hooks/, which is
  # where `lefthook install` writes its pre-commit shim — so a leftover
  # config from any prior hook manager (husky, pre-commit framework,
  # custom) makes the install silently no-op.
  if [ -n "${_hooks_path}" ]; then
    # Don't suppress unset failures with `|| true` — if the unset
    # fails (e.g. unwritable .git/config), the custom redirect stays
    # in place and the subsequent `lefthook install` is effectively
    # a no-op. The user needs to know that, not have it silently
    # swallowed.
    if ! git config --unset core.hooksPath; then
      echo "warning: failed to unset core.hooksPath (=${_hooks_path}); custom redirect still active — lefthook install will not take effect" >&2
    fi
  fi
  if ! "${_lefthook}" install >/dev/null; then
    echo "warning: lefthook install failed — pre-commit hook NOT wired" >&2
  fi
elif [ -x app-bin/pre-commit ]; then
  # Resolve the hooks dir via git plumbing rather than hardcoding
  # `.git/hooks`. In a git-worktree the per-worktree hooks live under
  # `.git/worktrees/<name>/hooks/`, and `.git` itself is a file (not
  # a directory), so `mkdir -p .git/hooks` fails. `--git-path hooks`
  # returns the right location in either layout. We also honor an
  # already-set `core.hooksPath` if present — fallback consumers
  # may have configured one deliberately. Use an absolute symlink
  # target so it resolves correctly from any hooks-dir depth.
  #
  # Best-effort: warn-and-continue on failure (matches the rest of
  # the script's continue-on-transient-errors stance — a failed
  # mkdir/symlink on an unusual worktree layout shouldn't abort the
  # entire dev-env setup).
  _hooks_dir="$(git config --get core.hooksPath 2>/dev/null || git rev-parse --git-path hooks)"
  # Best-effort with full diagnostics: don't suppress mkdir/ln stderr —
  # if either fails, the user needs the underlying error to fix it
  # (e.g. "Permission denied" pinpoints the actual issue).
  if ! mkdir -p "${_hooks_dir}"; then
    echo "warning: failed to mkdir -p \"${_hooks_dir}\" — pre-commit hook NOT wired" >&2
  elif ! ln -sf "${REPO_ROOT}/app-bin/pre-commit" "${_hooks_dir}/pre-commit"; then
    echo "warning: failed to symlink app-bin/pre-commit into \"${_hooks_dir}\" — pre-commit hook NOT wired" >&2
  fi
fi

# Cloud-only gate. Everything below is cloud-only — local sessions
# already have submodules, project deps, the NSS cert DB, etc., set up
# by the dev's machine.
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0

# --- 1. Universal git hygiene --------------------------------------------
# Cloud clones are shallow; restore submodule content and release tags.
# Submodule update is a no-op when in sync; tag fetch is one round-trip.

if [ -f .gitmodules ]; then
  git submodule update --init --recursive --quiet || true
fi
git fetch --tags --quiet origin || true

# --- 2. Project dep cache ------------------------------------------------
# Pick the right tool based on lockfile / manifest. Per stack, idempotent.

# Rust: cargo fetch with --locked so we don't silently mutate Cargo.lock.
if [ -f Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
  cargo fetch --locked --quiet || true
fi

# Go: `go mod download` populates the module cache without building.
# Cheap when the cache is already warm; ~free in steady state. Keep
# stderr visible so module-resolution / auth failures surface during
# debugging — `|| true` keeps us best-effort without silencing the why.
if [ -f go.mod ] && command -v go >/dev/null 2>&1; then
  go version
  go mod download || true
fi

# Node (npm/yarn/pnpm). We deliberately do NOT guard on `! -d node_modules`:
# the env-snapshot caches a node_modules paired with a previous branch's
# lockfile, and a feature branch that bumps the lockfile (Playwright is
# the canonical case) drifts silently. Re-installing when already in sync
# is ~2s; chasing a stale lockfile bug is hours. Pay the two seconds.
if [ -f package.json ]; then
  if [ -f package-lock.json ] && command -v npm >/dev/null 2>&1; then
    npm ci 2>/dev/null || npm install
  elif [ -f yarn.lock ] && command -v yarn >/dev/null 2>&1; then
    yarn install --frozen-lockfile 2>/dev/null || yarn install
  elif [ -f pnpm-lock.yaml ] && command -v pnpm >/dev/null 2>&1; then
    pnpm install --frozen-lockfile 2>/dev/null || pnpm install
  elif command -v npm >/dev/null 2>&1; then
    # No lockfile committed — repos like tree-sitter-lex deliberately
    # gitignore package-lock.json because the npm deps are dev-only
    # tooling (tree-sitter-cli, bats) and a committed lockfile would be
    # noise to bump. Without this branch, node_modules never gets
    # populated and any `npx <tool>` invocation fails.
    #
    # --no-package-lock matches the consumer's intent: they chose not
    # to commit a lockfile, so we shouldn't generate one in their
    # working tree just because we ran install.
    npm install --no-audit --no-fund --no-package-lock 2>/dev/null \
      || npm install --no-package-lock
  fi
fi

# Ruby / Bundler.
if [ -f Gemfile ] && command -v bundle >/dev/null 2>&1; then
  bundle install --quiet || true
fi

# Python / pip + venv. Triggered by any of the conventional manifests
# (pyproject.toml, requirements.txt, setup.py) so legacy projects are
# covered too.
#
# Run unconditionally on every session start — pip install is idempotent
# (sub-second when the deps are already in place), and the alternative
# (gating on `[ ! -d .venv ]`) means a half-installed .venv from a
# previous run persists across sessions, and re-running the script can
# never recover. mkdocs-lex's snapshot left .venv with only pip +
# setuptools and tests then failed with ModuleNotFoundError — the guard
# saw the directory, skipped reinstall, and nothing ever fixed it.
#
# Also: do NOT redirect install stderr to /dev/null. Swallowing the
# message is what made the partial-venv state silent in the first place.
# A loud warning to stderr surfaces real installation problems instead
# of papering over them.
if { [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f setup.py ]; } \
   && command -v python3 >/dev/null 2>&1; then
  # Gate venv creation on `.venv/bin/pip` being executable, not just
  # `.venv/` existing. A previous run can leave the directory in place
  # with pip missing (interrupted mid-snapshot, broken extraction);
  # checking pip directly recovers from that. Warn loudly when the
  # creation itself fails — otherwise the next gate silently skips all
  # pip work and the agent debugs a missing-module mystery.
  if [ ! -x .venv/bin/pip ]; then
    if ! python3 -m venv .venv; then
      echo "warning: python3 -m venv .venv failed — pip installs will be skipped" >&2
    fi
  fi
  if [ -x .venv/bin/pip ]; then
    .venv/bin/pip install --upgrade pip --quiet || true
    if [ -f pyproject.toml ]; then
      # No fallback to plain `.` — modern pip treats `[dev]` against a
      # pyproject without that extra as a warn-and-continue (still
      # installs base, exits 0). A genuine failure means a real dep
      # can't resolve, and falling back to `.` would silently leave
      # the venv with base installed but dev-extras (pytest etc)
      # missing. Surface the failure instead.
      .venv/bin/pip install -e '.[dev]' --quiet \
        || echo "warning: editable install failed — tests will not run (see pip output above)" >&2
    elif [ -f requirements.txt ]; then
      .venv/bin/pip install -r requirements.txt --quiet \
        || echo "warning: requirements install failed — tests will not run" >&2
    elif [ -f setup.py ]; then
      .venv/bin/pip install -e . --quiet \
        || echo "warning: editable install failed — tests will not run" >&2
    fi

    # Expose venv-installed CLIs on the agent's bare PATH.
    #
    # The cloud Bash tool runs non-interactive shells whose PATH is
    # fixed at session start and does NOT include
    # ${REPO_ROOT}/.venv/bin. ~/.bashrc returns early for non-
    # interactive shells (`[ -z "$PS1" ] && return`), so PATH fixes
    # there are unreachable. The agent's `subprocess.run(['mkdocs',
    # …])` (or any test that shells out to a venv CLI) resolves the
    # command against the agent's PATH and gets FileNotFoundError.
    #
    # Symlink every executable in .venv/bin (except the
    # python/pip/activate family — those would shadow system commands
    # or break venv internals) into ${HOME}/.local/bin/, which IS on
    # the agent's PATH (it's where uv / pipx / similar Python tooling
    # already drops entry points). Idempotent — `ln -sf` overwrites
    # stale symlinks pointing into a previous session's path.
    #
    # Consumers that install ADDITIONAL CLIs from project-local extras
    # (pinned-binary downloads from GitHub releases, etc) should drop
    # them directly into ${HOME}/.local/bin rather than .venv/bin, so
    # they're discoverable on the same PATH without needing a second
    # symlink pass.
    if [ -d .venv/bin ]; then
      # Create ~/.local/bin if missing — env/setup.sh doesn't and Ubuntu
      # cloud images don't ship it by default in fresh users. The
      # directory is on the default PATH for any login that picks up
      # ~/.profile, but we still need it to exist before we ln into it.
      mkdir -p "${HOME}/.local/bin"
      for _venv_bin in .venv/bin/*; do
        # Require both regular file (after symlink resolution) AND
        # executable bit. `-x` alone matches directories, which would
        # produce a useless dangling symlink if the glob ever did.
        # Two separate guards, not `A && B || continue`: older shellcheck
        # (Ubuntu's 0.9/0.10, which consumers run in CI) flags that form as
        # SC2015. Equivalent behaviour, clean on every shellcheck version.
        [ -f "${_venv_bin}" ] || continue
        [ -x "${_venv_bin}" ] || continue
        # Parameter expansion avoids forking basename per iteration.
        _name="${_venv_bin##*/}"
        case "${_name}" in
          python|python[0-9]*|pip|pip[0-9]*|activate*|easy_install*|wheel|wheel[0-9]*)
            continue
            ;;
        esac
        # `--` defends against (pathological) filenames starting with -;
        # `|| true` matches the script's best-effort policy — a single
        # permission hiccup shouldn't abort the rest of session setup.
        ln -sf -- "${REPO_ROOT}/.venv/bin/${_name}" "${HOME}/.local/bin/${_name}" || true
      done
    fi
  fi
fi

# --- 2.5. Chromium NSS DB cert import ------------------------------------
# Cloud sessions route HTTPS through an "Anthropic sandbox-egress…CA"
# proxy that re-signs every leaf cert. Chromium on Linux ignores the
# OpenSSL bundle and reads its own NSS DB at ~/.pki/nssdb — without
# the CA imported there, every HTTPS resource an Electron / Playwright
# test loads is rejected with ERR_CERT_AUTHORITY_INVALID. The e2e
# harness's runtime-error fixture surfaces that as a `console.error`
# and the test auto-fails.
#
# Cert layouts seen in the cloud env (probe both):
#   (A) Historical (~pre-2026-05): the sandbox-egress CA was
#       concatenated into the system bundle
#       /etc/ssl/certs/ca-certificates.crt alongside public roots.
#   (B) Current (2026-05+): the CA ships as standalone PEMs at
#       /etc/ssl/certs/swp-ca-{production,staging}.pem; it is NOT
#       written into the system bundle, so the old layout-A grep gate
#       silently misses it and the NSS DB is never populated. `curl`
#       and Node still work because they read the bundle directly via
#       their own paths — only Chromium / Electron is affected.
#
# Strategy: collect candidate PEMs from both layouts into a scratch
# dir, then run the subject-match-and-import loop over the union.
# Fast-path: skip everything if neither layout has any matching cert
# (non-cloud Linux box). Idempotent — `certutil -L -n <nick>` short-
# circuits the `-A` import once a cert is present.
#
# Gated on `certutil` AND `openssl` existing (the loop forks openssl
# per cert to extract the subject); both are env-level state on cloud
# sessions but may be absent locally.
if [ "$(uname -s)" = "Linux" ] \
   && command -v certutil >/dev/null 2>&1 \
   && command -v openssl >/dev/null 2>&1; then
  # Subshell scopes the EXIT trap so cleanup is reliable under `set -e`
  # AND doesn't overwrite a process-wide EXIT trap. The subshell exits
  # when this block finishes, the trap fires, the tmp dir is gone — no
  # leak even if awk/cp/openssl error out below.
  #
  # The trailing `|| true` matches the script's stated philosophy
  # (line ~19: errors are best-effort). A cert-import failure shouldn't
  # abort the rest of the dev-env bootstrap.
  (
    _ca_tmp="$(mktemp -d)"
    trap 'rm -rf "${_ca_tmp}"' EXIT
    _found=0

    # Layout A: split the system bundle into per-cert PEMs if it contains
    # any Anthropic CA. Cheap grep gate avoids the awk fork on non-cloud
    # Linux boxes (where the bundle has no matches).
    if [ -f /etc/ssl/certs/ca-certificates.crt ] \
       && grep -q 'Anthropic' /etc/ssl/certs/ca-certificates.crt 2>/dev/null; then
      awk -v sandbox_dir="${_ca_tmp}" '
        /-----BEGIN CERTIFICATE-----/ { n++; fn = sandbox_dir "/bundle_" n ".pem"; in_cert = 1 }
        in_cert                       { print > fn }
        /-----END CERTIFICATE-----/   { in_cert = 0; close(fn) }
      ' /etc/ssl/certs/ca-certificates.crt
      _found=1
    fi

    # Layout B: copy standalone swp-ca-*.pem files into the scratch dir.
    # The glob may be unexpanded if no file matches; guard with -f.
    for _pem in /etc/ssl/certs/swp-ca-*.pem; do
      [ -f "${_pem}" ] || continue
      cp "${_pem}" "${_ca_tmp}/$(basename "${_pem}")"
      _found=1
    done

    if [ "${_found}" = "1" ]; then
      _nssdb="${HOME}/.pki/nssdb"
      mkdir -p "${_nssdb}"
      if [ ! -f "${_nssdb}/cert9.db" ]; then
        certutil -d "sql:${_nssdb}" -N --empty-password >/dev/null 2>&1 || true
      fi
      for _pem in "${_ca_tmp}"/*.pem; do
        [ -f "${_pem}" ] || continue
        _subject="$(openssl x509 -in "${_pem}" -noout -subject 2>/dev/null || true)"
        case "${_subject}" in
          *Anthropic*sandbox-egress*)
            _nick="$(printf '%s' "${_subject}" | sed -nE 's/.*CN *= *([^,]+).*/\1/p')"
            [ -n "${_nick}" ] || continue
            if ! certutil -d "sql:${_nssdb}" -L -n "${_nick}" >/dev/null 2>&1; then
              certutil -d "sql:${_nssdb}" -A -t "C,," -n "${_nick}" -i "${_pem}" >/dev/null 2>&1 || true
            fi
            ;;
        esac
      done
    fi
  ) || true
fi

# --- 4. Per-repo hook -------------------------------------------------------
_hook="${REPO_ROOT}/app-bin/post-setup-hook.sh"
if [ -f "${_hook}" ]; then
  if [ -x "${_hook}" ]; then
    "${_hook}"
  else
    echo "warning: ${_hook} exists but is not executable; skipping" >&2
  fi
fi
