#!/usr/bin/env bash
# scripts/setup-dev-env.sh — per-session dev-environment setup, invoked by
# the SessionStart hook in .claude/settings.json.
#
# Source of truth: arthur-debert/release templates/setup-dev-env.sh.
# To re-sync, copy this file verbatim over the consumer's
# scripts/setup-dev-env.sh. (The gh-repo-setup skill does not currently
# route this top-level template; it only handles per-stack trees under
# templates/<stack>/.)
# Repos that need project-specific extras (Xvfb daemon, pinned-binary
# fetch, extra rustup targets, etc.) append them below the marker at the
# bottom — anything above it is rsync'd from the template.
#
# Cloud-only: local sessions exit early (devs already have their env).
# Detects stack by filesystem signals — handles rust, node, ruby, python,
# and consumers with no project deps (just lefthook / hand-rolled hook
# wiring).
#
# Idempotent — safe to re-run. Errors are best-effort: a failure in one
# step does not abort the rest (transient registry hiccups shouldn't
# block the lefthook install).

set -euo pipefail

# Cloud-only gate. Local sessions already have their env set up.
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "${REPO_ROOT}"

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
    # symlink pass below the marker.
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
        [ -f "${_venv_bin}" ] && [ -x "${_venv_bin}" ] || continue
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

# --- 3. Pre-commit hook wiring -------------------------------------------
# Default: lefthook (binary installed at env-setup time). Fallback for
# repos that ship a hand-rolled scripts/pre-commit instead (zed-lex,
# tree-sitter-lex pattern): symlink it into .git/hooks/.

if [ -f lefthook.yml ] && command -v lefthook >/dev/null 2>&1; then
  if ! lefthook install >/dev/null; then
    echo "warning: lefthook install failed — pre-commit hook NOT wired" >&2
  fi
elif [ -x scripts/pre-commit ]; then
  mkdir -p .git/hooks
  ln -sf ../../scripts/pre-commit .git/hooks/pre-commit
fi

# --- 4. Project-local extras ---------------------------------------------
# Everything above this marker is the canonical cross-repo setup-dev-env.sh
# from arthur-debert/release templates/setup-dev-env.sh. Do NOT modify it
# in-place; consumers append project-specific steps BELOW this marker.
# (See e.g. lex-fmt/lexed for an Xvfb start, lex-fmt/nvim for pinned-bin
# fetches.)
#
# No trailing `exit 0` — bash exits 0 on EOF when `set -euo pipefail`
# succeeded. Adding one here would make appended extras unreachable.
