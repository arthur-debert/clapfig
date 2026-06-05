---
name: release-issue-relay
description: "Escalate infrastructure friction (workflow failures, ruleset misbehaviors, broken policy templates, helper-script bugs) from a consumer repo to the canonical `arthur-debert/release` repo. Files a GitHub issue at release with auto-collected reproduction context (repo, branch, PR, recent workflow run, workaround applied). Searches recent release issues first and comments on a matching one rather than filing a duplicate — the comment count is the signal of recurrence. Use ONLY for infra issues that the consumer cannot fix in place; do not use for code-quality nits, project-specific test failures, or anything fixable inside the consumer repo. Triggered by: 'this workflow fails in a way I can't fix here', 'the release-loop policy is misbehaving', 'an infra script errors', 'the cloud env is missing something it should provide'."
---

# release-issue-relay

Portable port of `release/bin/gh-release-issue`, with dedupe-via-search added on top. This skill is the **read-side** half of the sustainability loop: when an agent at a consumer repo hits infrastructure friction, unblock locally and then escalate the symptom upstream so the fix lives at the source.

## The hard rule about scope

This skill is **only** for infrastructure issues that the consumer repo cannot fix in place. The canonical signal: *"I applied a local workaround to keep working, but the underlying problem is in arthur-debert/release, not here."*

| Use it for | Do not use it for |
|---|---|
| `copilot-review.yml` workflow runs SUCCESS but Copilot is not actually attached as reviewer | A Copilot review comment that says "rename this variable" |
| `gh-copilot-wait` times out on a non-draft PR with no obvious reason | A test failure specific to the code in this PR |
| `apply-ruleset` / `gh-repo-setup` rejects a check name that exists in the workflow | A policy file conflict that's clearly intentional per-repo customization |
| `rust-cli` reusable workflow errors out in a way that looks like infra, not project code | A `cargo clippy` warning that needs a code fix |
| `sweep-github-policy` produces a conflict that looks like a template bug | A pre-commit hook failure specific to the staged diff |
| A skill (this one, `pr-review-respond`, `gh-repo-setup`) misbehaves in a way you can't fix by editing the local invocation | A skill triggering on the wrong description match (that's a description tweak, not a bug) |

If you're unsure which side a problem falls on, ask the user before filing.

## Prerequisites

- **`gh` CLI authenticated.** In cloud sessions, `GH_TOKEN` must include `arthur-debert/release` in the allowed-repositories set with `Issues: Read and write`. Clones don't need auth (release is public), but issue creation and comments do. If the PAT doesn't include release, the skill errors at the file-issue step — that's the signal to update the PAT.
- Bash 4+; `gh` and `jq` available.

## What gets collected

The skill auto-gathers from the current session:

- **Consumer repo** — `gh repo view --json nameWithOwner -q .nameWithOwner`
- **Branch** — `git branch --show-current`
- **Commit SHA** — `git rev-parse --short HEAD` (short SHA at the time of report; matters because the branch tip moves as work continues)
- **PR** (if the branch has one) — `gh pr list --head "$branch" --json number -q '.[0].number'`
- **Recent workflow run** — for the component being reported, filtered to the **current branch** so the URL points at the actual incident. We try both `.yml` and `.yaml` extensions and fall through gracefully if neither matches.

You supply two strings:

- **`COMPONENT`** — one of the canonical buckets (see Components section); free-form is allowed but standardized buckets help triage.
- **`SYMPTOM`** — one-line description, used as the issue title suffix.

Plus the agent's own write-up in the body: the workaround applied locally, suspected cause, anything else useful for triage.

## Step 1: dedupe search

Before filing, look for an existing open issue that describes the same problem. Comment on it rather than filing a duplicate — the comment count is the signal of recurrence, and the maintainer gets one tracked thread instead of N.

```sh
set -euo pipefail

# Set these before running. The snippet doesn't take positional args (avoids
# `set -u` aborting on bare $1 / $2 when an agent pastes it into a fresh shell).
COMPONENT="copilot-review"   # one of the canonical buckets — see "Components" below
SYMPTOM="workflow runs SUCCESS but requested_reviewers stays empty"

# Pull the last 30 OPEN issues at release. The maintainer rarely lets the
# backlog grow past that; if it does, expand --limit.
# The component is double-quoted inside the search string so the bracketed
# token is treated as a single search term even if it ever contained spaces.
RECENT_ISSUES=$(gh issue list --repo arthur-debert/release \
  --state open --limit 30 \
  --json number,title,url,createdAt \
  --search "\"[${COMPONENT}]\" in:title")

echo "$RECENT_ISSUES" | jq -r '.[] | "  #\(.number) (\(.createdAt[:10]))  \(.title)"'
```

Read the output. Decide:

- **Exact match on `[component] symptom`** → comment on that one (step 2a).
- **Same component, different but clearly-the-same symptom** (e.g. "Copilot review workflow ran SUCCESS but no reviewer attached" vs the one you'd file: "workflow runs SUCCESS but requested_reviewers stays empty") → comment on that one.
- **No related open issue** → file a new one (step 2b).

When in doubt, prefer commenting — it's easier to split a clustered issue than to merge duplicates later. The maintainer can re-triage if your comment turns out to be a different problem.

## Step 2a: comment on an existing issue

If you found a match in step 1, comment with the current incident's context:

```sh
set -euo pipefail

EXISTING_ISSUE_NUMBER=42   # replace with the #N you matched in step 1

# Auto-collect the consumer context. Same shape as for new issues.
CONSUMER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
BRANCH=$(git branch --show-current 2>/dev/null || echo "(unknown)")
COMMIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "(unknown)")
PR_NUMBER=$(gh pr list --head "$BRANCH" --json number -q '.[0].number // empty' 2>/dev/null || true)
PR_URL=""
[ -n "$PR_NUMBER" ] && PR_URL="https://github.com/$CONSUMER_REPO/pull/$PR_NUMBER"

# Look up the most recent workflow run on THIS branch (not most-recent
# anywhere), so the URL points at the actual incident. Try .yml first,
# fall back to .yaml. `|| true` keeps the chain alive if neither exists.
recent_run_for_workflow() {
  local wf=$1
  gh run list --workflow "$wf" --branch "$BRANCH" --limit 1 \
    --json url -q '.[0].url // empty' 2>/dev/null || true
}
RECENT_RUN=""
case "$COMPONENT" in
  copilot-review)
    RECENT_RUN=$(recent_run_for_workflow copilot-review.yml)
    [ -z "$RECENT_RUN" ] && RECENT_RUN=$(recent_run_for_workflow copilot-review.yaml) ;;
  rust-cli-release)
    RECENT_RUN=$(recent_run_for_workflow release.yml)
    [ -z "$RECENT_RUN" ] && RECENT_RUN=$(recent_run_for_workflow release.yaml) ;;
esac

BODY=$(cat <<EOF
Also hit on **${CONSUMER_REPO}**, branch \`${BRANCH}\` @ \`${COMMIT_SHA}\`.

${PR_URL:+- PR: ${PR_URL}
}${RECENT_RUN:+- Workflow run: ${RECENT_RUN}
}- Local workaround: <describe what you did to unblock>
- Suspected cause: <if you have a hypothesis; "none" is fine>

_Filed via release-issue-relay (comment on existing)._
EOF
)

jq -n --arg body "$BODY" '{body: $body}' \
  | gh api "repos/arthur-debert/release/issues/${EXISTING_ISSUE_NUMBER}/comments" \
      -X POST --input -
```

Replace `<describe what you did to unblock>` and `<if you have a hypothesis>` with real content before running.

## Step 2b: file a new issue

If step 1 found nothing relevant, file a fresh issue:

```sh
set -euo pipefail

CONSUMER_REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
BRANCH=$(git branch --show-current 2>/dev/null || echo "(unknown)")
COMMIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "(unknown)")
PR_NUMBER=$(gh pr list --head "$BRANCH" --json number -q '.[0].number // empty' 2>/dev/null || true)
PR_URL=""
[ -n "$PR_NUMBER" ] && PR_URL="https://github.com/$CONSUMER_REPO/pull/$PR_NUMBER"

# Branch-scoped recent run with .yml/.yaml fallback (see step 2a's helper).
recent_run_for_workflow() {
  local wf=$1
  gh run list --workflow "$wf" --branch "$BRANCH" --limit 1 \
    --json url -q '.[0].url // empty' 2>/dev/null || true
}
RECENT_RUN=""
case "$COMPONENT" in
  copilot-review)
    RECENT_RUN=$(recent_run_for_workflow copilot-review.yml)
    [ -z "$RECENT_RUN" ] && RECENT_RUN=$(recent_run_for_workflow copilot-review.yaml) ;;
  rust-cli-release)
    RECENT_RUN=$(recent_run_for_workflow release.yml)
    [ -z "$RECENT_RUN" ] && RECENT_RUN=$(recent_run_for_workflow release.yaml) ;;
esac

TITLE="[${COMPONENT}] ${SYMPTOM}"

BODY=$(cat <<EOF
**Component:** ${COMPONENT}
**Reported from:** ${CONSUMER_REPO}
**Branch:** \`${BRANCH}\` @ \`${COMMIT_SHA}\`
${PR_URL:+**PR:** ${PR_URL}
}${RECENT_RUN:+**Workflow run:** ${RECENT_RUN}
}
## Symptom

${SYMPTOM}

## Local workaround applied

<describe what you did to keep working — e.g. "manually requested Copilot via gh pr edit --add-reviewer @copilot", "hardcoded the check name in the ruleset payload", etc.>

## Suspected cause

<your hypothesis, or "none" if you don't have one yet>

## Suggested fix

<if obvious; "none" / "unknown" if not>

---

_Filed via release-issue-relay. The reporting agent should follow up with logs and any reproduction steps that aren't obvious from the links above._
EOF
)

# Build the JSON payload via jq (avoids quoting hazards) and POST via gh api.
# The `consumer-filed` label is the marker the fleet inbox (release-inbox,
# release#348 Phase C) filters on — title-prefix search is best-effort, a label
# is reliable. It exists on arthur-debert/release as part of the inbox contract.
jq -n --arg title "$TITLE" --arg body "$BODY" \
  '{title: $title, body: $body, labels: ["consumer-filed"]}' \
  | gh api "repos/arthur-debert/release/issues" -X POST --input - \
  | jq -r '"filed: \(.html_url)"'
```

Replace the `<...>` placeholders before running.

## Components

These are the canonical buckets; the title prefix is what step 1's `--search` filters on. Stay within this list when possible — new components should be added deliberately:

| Component | Covers |
|---|---|
| `copilot-review` | `copilot-review.yml` workflow misbehaves, Copilot not attached as reviewer, Auto-fix not firing |
| `pr-review-loop` | `gh-copilot-{on,off,wait}`, `gh-pr-resolve-thread`, `gh-pr-checks-wait` and related helpers |
| `rust-cli-release` | `rust-cli.yml` reusable release workflow (test, build, publish, homebrew, wasm) |
| `ruleset` | `apply-ruleset` / branch protection / required check detection |
| `sweep-policy` | `sweep-github-policy` / per-stack templates / destination mapping |
| `gh-repo-setup` | The portable repo-setup skill (this one's sibling) |
| `install-token` | `install-release-token`, `install-release-secrets`, secret-name conventions |
| `audit` | `audit-portfolio`, `audit-repo`, `audit-smoke-test` |
| `cloud-env` | Setup script, `~/.claude/skills/` install, `~/.claude/CLAUDE.md`, the env distribution mechanism |
| `skill` | A skill itself misbehaves (be specific in the symptom about which skill) |
| `other` | Anything not covered above; bias toward picking a more specific bucket if applicable |

## Pitfalls

- **PAT scope is the first thing to verify if `gh api ... /issues` returns 403.** Cloud-env PATs are typically scoped to the related-repo group only; release isn't usually in that list at first. Add `arthur-debert/release` with `Issues: Read and write` to the PAT.
- **Don't file the same issue from N consumer sessions in N minutes.** The dedupe search is best-effort — if you opened a session for phos-core and another for phos-app and hit the same infra bug in both, the second session may file before the first one's issue has propagated through GitHub search indexing (which can lag a minute or two). If you suspect that's happened, comment on whichever issue came in first and close the duplicate.
- **`gh issue list --search` is best-effort substring matching.** It looks at title + body. If you want strict title-prefix matching, post-filter with jq, using double quotes so `$COMPONENT` actually expands: `--jq ".[] | select(.title | startswith(\"[${COMPONENT}]\"))"`.
- **Non-infra escalation is the most common misuse.** If you're escalating a code-quality nit or a test failure specific to the PR you're working on, the right loop is the PR review thread, not this skill. The PR Body and `pr-review-respond`'s pushback patterns are where that conversation belongs.
- **The body's `<...>` placeholders are real.** A filed issue with literal `<describe what you did>` text is a bug in the agent invoking the skill, not in the skill itself. Substitute meaningful text before the `jq | gh api` step.

## Related

- `gh-release-issue` — the sibling CLI, used by humans and agents that have it on `$PATH`. Since release#348 it ships to consumers under `templates/commons/bin/` (synced into each consumer's `bin/`), so it's reachable inside a managed repo, not just on the maintainer's machine. Same body shape; no dedupe step.
- The broader sustainability loop this skill is half of. The other half (write-side: scheduled portfolio audit routine at release) is Phase 4b.
