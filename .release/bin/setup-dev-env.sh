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

# npm globals: lefthook + prettier + markdownlint. Installed before the hook
# wiring below so `lefthook install` finds the binary.
if ! command -v lefthook >/dev/null 2>&1 \
  || ! command -v prettier >/dev/null 2>&1 \
  || ! command -v markdownlint >/dev/null 2>&1; then
  if command -v npm >/dev/null 2>&1; then
    npm install -g lefthook prettier markdownlint-cli >/dev/null 2>&1 || true
  fi
fi
command -v lefthook >/dev/null 2>&1 || _warn_unarmed lefthook
command -v prettier >/dev/null 2>&1 || _warn_unarmed prettier
command -v markdownlint >/dev/null 2>&1 || _warn_unarmed markdownlint

# ruff, pinned to the CI version so local and CI never disagree on findings.
# Modern Debian/Ubuntu (PEP 668) reject a global pip install without
# --break-system-packages; try that first, fall back to a plain install (older
# distros / venvs reject the flag). Install stderr is left visible — a failure
# reason should surface, not be swallowed.
_RUFF_VERSION="0.15.12"
if ! command -v ruff >/dev/null 2>&1; then
  if command -v pip3 >/dev/null 2>&1; then
    pip3 install --quiet --break-system-packages "ruff==${_RUFF_VERSION}" >/dev/null \
      || pip3 install --quiet "ruff==${_RUFF_VERSION}" >/dev/null || true
  elif command -v brew >/dev/null 2>&1; then
    brew install ruff >/dev/null || true
  fi
fi
command -v ruff >/dev/null 2>&1 || _warn_unarmed ruff

# yamllint — the consumer gate's YAML check (commons lefthook fragment). Like
# ruff it's a pip tool: --break-system-packages first (PEP 668), plain pip
# fallback, then brew. Install stderr stays visible for diagnostics.
if ! command -v yamllint >/dev/null 2>&1; then
  if command -v pip3 >/dev/null 2>&1; then
    pip3 install --quiet --break-system-packages yamllint >/dev/null \
      || pip3 install --quiet yamllint >/dev/null || true
  elif command -v brew >/dev/null 2>&1; then
    brew install yamllint >/dev/null || true
  fi
fi
command -v yamllint >/dev/null 2>&1 || _warn_unarmed yamllint

# System tools: shellcheck + actionlint (brew on macOS, apt on Linux). `sudo -n`
# (non-interactive) so a password prompt fails fast instead of hanging a
# session-start hook; install stderr stays visible for diagnostics.
for _gate_tool in shellcheck actionlint; do
  command -v "${_gate_tool}" >/dev/null 2>&1 && continue
  if command -v brew >/dev/null 2>&1; then
    brew install "${_gate_tool}" >/dev/null || true
  elif command -v apt-get >/dev/null 2>&1; then
    sudo -n apt-get install -y "${_gate_tool}" >/dev/null || true
  fi
  command -v "${_gate_tool}" >/dev/null 2>&1 || _warn_unarmed "${_gate_tool}"
done

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

# --- 0.1. Pre-commit hook wiring (BOTH local and cloud) -----------------
# Wiring `.git/hooks/pre-commit` is per-clone state — every fresh clone
# (and cloud snapshot) starts without it, so we wire it on every session
# and in both contexts. Runs ABOVE the cloud-only gate because skipping
# it locally is what produces the "agents are still running husky"
# symptom: lefthook never gets installed, husky's old wiring keeps
# firing.
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

if [ -f lefthook.yml ] && [ -n "${_lefthook}" ]; then
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

# --- 0.2. Pull-model: self-update release_core from the published wheel --
# The north star (ADR-0003): repos AUTO-UPDATE on session start, retiring the
# hand-run `orc propagate` treadmill. The boot resolver `install-release-core`
# resolves the latest release wheel, pip-installs it (--force-reinstall --no-deps
# — the wheel version is static, so `-U` would skip it), THEN runs `release-core
# init` itself (it locates the just-installed console-script across venv/--user/
# system layouts). One command does the whole boot. Runs in BOTH local and cloud
# (above the cloud-only gate) — auto-update is the whole point.
#
# BEST-EFFORT, never aborts the session: every call is `|| warn`, and init
# failure inside the resolver is itself best-effort. The committed config already
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
# Install target (so the console-scripts land on PATH): a repo `.venv` if one is
# already present (its bin/ is symlinked onto PATH by §2 / dodot), else `--user`
# → the user site (ensured on PATH below). `--break-system-packages` is tried
# first for PEP-668 system pythons, with a plain `--user` fallback (older pip
# rejects the flag), mirroring the ruff/yamllint blocks above.
_resolver=""
if [ -x "${REPO_ROOT}/bin/install-release-core" ]; then
  _resolver="${REPO_ROOT}/bin/install-release-core"
elif command -v install-release-core >/dev/null 2>&1; then
  _resolver="install-release-core"
fi
if [ -n "${_resolver}" ]; then
  mkdir -p "${HOME}/.local/bin"
  if [ -x .venv/bin/python ]; then
    PYTHON="${REPO_ROOT}/.venv/bin/python" "${_resolver}" \
      || echo "warning: install-release-core (.venv) failed — release_core not updated this session" >&2
  else
    "${_resolver}" --user --break-system-packages \
      || "${_resolver}" --user \
      || echo "warning: install-release-core (--user) failed — release_core not updated this session" >&2
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
