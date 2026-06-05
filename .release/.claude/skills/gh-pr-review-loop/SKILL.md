---
name: gh-pr-review-loop
description: "Drive a PR to ready-for-human-merge in any repo managed by `arthur-debert/release`. Start with `release-core pr status` to see where the PR stands (one lifecycle state + next action), then push, request the Copilot review, wait, triage comments, resolve threads, and stop when mergeable — the human does the final merge. Use when opening a PR, checking where a PR stands, waiting on or triaging Copilot review feedback, or driving a PR toward merge-readiness. Triggered by: `gh pr create`, 'check PR status', 'where does this PR stand', requesting a Copilot review, or processing review comments."
---

# gh-pr-review-loop

The canonical PR loop for repos onboarded to the GitHub-side standardization (ruleset + Copilot auto-trigger + policy files). Worked out iteratively across 16 PRs in April 2026 — these instructions encode what was learned.

## When to use this skill

- Working on a PR (or about to open one) in any onboarded repo under `arthur-debert/*` or `lex-fmt/*` (run `release-core audit` to confirm — onboarded repos pass `ruleset` + `copilot_review`).
- About to merge a PR on one of those repos
- Triaging Copilot review feedback
- Onboarding a new repo (use the **Onboarding a new repo** section at the end)
- Auditing or smoke-testing the loop's health (use `release-core admin repos audit` / `release-core admin smoke-test`)

## The repos in scope

To get the current authoritative list:

```sh
release-core admin repos audit --only-failing  # reads managed-repos.yaml (the hardcoded fleet; no discovery)
```

All have:

- `main` branch protected by a `main-branch-protection` ruleset (PR required, check pass required, 0 reviews, no force-push, no delete, linear history)
- `.github/workflows/copilot-review.yml` calling `arthur-debert/release/.github/workflows/copilot-review.yml@v1` on `pull_request: [opened, ready_for_review]`, passing `gh_token: ${{ secrets.RELEASE_TOKEN }}` via `secrets:`
- `RELEASE_TOKEN` secret set (propagated by `release-core admin secrets token`)
- `.github/copilot-instructions.md`, `CODEOWNERS`, `dependabot.yml`, `pull_request_template.md` (rust stacks); some non-rust stacks have only the stack-agnostic subset (CODEOWNERS + dependabot.yml + copilot-review.yml) until per-stack templates are written.

Out of scope (commented out in `managed-repos.yaml`, with reasons): `arthur-debert/{homebrew-tools,simple-gal-action}` and inactive repos like `treex`.

## The helpers

Single home: **`~/h/release/bin/`** — both the policy/setup tools and the day-to-day PR-loop helpers. On `$PATH` via dodot's path handler (`dodot up release`). Source of truth is `arthur-debert/release` so a fix in any helper propagates to every machine that pulls the repo.

(Historical note: the PR-loop helpers used to live in `~/h/dotfiles/gh/bin/` while policy tooling lived in `release/bin/`. Consolidated 2026-05-05 — if you find an older copy in `dotfiles/gh/bin/`, it's stale; the canonical version is in `release/bin/`.)

| Command | What it does |
|---|---|
| `release-core pr status [<pr>] [--json]` | **The orient step.** Reads the PR once and reports one lifecycle state (`REVIEWS_PENDING` / `ADDRESSING` / `REVIEWED` / `VALIDATING` / `READY` / `BLOCKED`) plus the next action. Reviewer-agnostic (Copilot required, Gemini best-effort), with circuit breakers folded in. Read-only. Resolves the current branch's PR if `<pr>` is omitted. See "Orienting with release-core pr status" below. |
| `release-core pr copilot on <pr>` | Request Copilot review (`gh pr edit --add-reviewer @copilot` — goes through GraphQL with the bot's real node_id; the `requested_reviewers` REST POST silently no-ops with `reviewers[]=copilot-pull-request-reviewer[bot]`). |
| `release-core pr copilot off <pr>` | Remove Copilot reviewer. |
| `release-core pr copilot wait <pr>` | Block until Copilot posts a review on the PR's current head SHA. 7m initial sleep, 2m polling, 30m hard cap. Exit 0 = posted; 2 = timeout. |
| `release-core pr copilot review <pr>` | Composite: on + wait + print review body and inline comments. |
| `release-core pr resolve-thread <pr> <comment-id>` | Resolve the review thread containing the given comment via GraphQL `resolveReviewThread`. Idempotent — already-resolved threads exit 0 without complaint. Use after you fix-and-push or reply with rationale (see step 5 below). |
| `release-core pr checks-wait <pr> [extra gh args...]` | Wait for all required checks to pass (or fail). Exit 0 = all pass; 1 = any fail. |
| `release-core issue file <component> <symptom>` | File a bug at `arthur-debert/release` from inside any consumer repo. Auto-collects current repo, branch, PR, and recent workflow run for reproduction context. Use whenever the loop misbehaves in a way the consumer can't fix locally (see "When the loop misbehaves" below). |
| `release-core admin policy ruleset [--dry-run] [--checks ...]` (retired flat: `apply-ruleset`) | Apply the canonical main-branch-protection ruleset to the current repo. Auto-detects required checks from the latest default-branch run of each PR-trigger workflow (handles matrix expansion + `name:` overrides). |
| `release-core admin policy sweep [--force]` (retired flat: `sweep-github-policy`) | Drop the canonical `.github/` policy files into the current repo (CODEOWNERS, dependabot.yml, copilot-instructions.md, pull_request_template.md, workflows/copilot-review.yml). Reports created/updated/ok/conflict per file. |
| `release-core detect-kind [<dir>]` | Identify the project Kind (rust, electron, vsce-ext, nvim-plugin, tree-sitter, brew-tap, github-action, static-site). |
| `release-core admin secrets token` (retired flat: `install-release-token`) | Read a classic PAT from stdin and propagate as `RELEASE_TOKEN` secret to every onboarded repo. Verifies persistence per repo (older silent-fail mode is fixed). Required after PAT rotation. |
| `release-core admin policy dependabot [--repos ...]` (retired flat: `enable-dependabot-security`) | Enable Dependabot vulnerability-alerts + auto-fix on every onboarded repo via the API toggle. |
| `release-core audit [--repo <r>]` (retired flat: `audit-repo`) | Per-repo readout: ruleset, RELEASE_TOKEN, copilot-review pointer, CODEOWNERS, dep_security, dep_policy, ci_main_green, private go module auth. PASS/FAIL/WARN per row. |
| `release-core admin repos audit [--only-failing]` (retired flat: `audit-portfolio`) | Loop the per-repo audit over the `managed-repos.yaml` fleet (the hardcoded source of truth; no discovery). Summary table + detail of problem repos. |
| `release-core admin smoke-test <repo>` (retired flat: `audit-smoke-test`) | Open a no-op PR, verify Copilot fires + checks trigger + Copilot is added as reviewer (timeline event), close the PR. Real end-to-end verification. Use after a config change to confirm the loop actually still works. |

Templates live at `~/h/release/templates/<stack>/`; ruleset JSON at `~/h/release/rulesets/main-protection.json.tmpl`. Scripts resolve these relative to their own location, so no XDG/symlink indirection is needed.

## The PR flow

This is the canonical sequence when driving a feature branch through the loop:

```text
1. branch + change + commit
2. push
3. open PR (gh pr create) — NEVER pass --draft unless the user explicitly asked
   → ruleset enforces PR-only (no direct push to main)
   → copilot-review.yml auto-triggers Copilot review request
4. (wait) Copilot posts review at ~7m typical
5. triage Copilot comments
6. push fixups (CI re-runs; do NOT re-request Copilot if the round was minor)
7. wait for checks
8. STOP. PR is mergeable, agent's job is done. The user does the final read and merges.
   Merge only on explicit authorization ("merge it", "go ahead and merge").
9. ALWAYS close with the structured report block (see "The final-report contract").
```

### Orienting with `release-core pr status`

Instead of piecing the PR's state together by hand on every wake — which review landed? are the threads resolved? are checks green? is it mergeable? — ask one command:

```sh
release-core pr status <PR>          # human-readable; add --json to parse
```

It reads the PR once and reports exactly one lifecycle state plus the next action. It *orchestrates* the primitives below — it does not replace them. Map the state to where you are in the flow:

| State | Meaning | What to do |
|---|---|---|
| `REVIEWS_PENDING` | a required reviewer (Copilot) hasn't finished | step 4 — `release-core pr copilot wait` |
| `ADDRESSING` | reviews in, open threads remain | steps 5–5b — triage, fix/reply, resolve |
| `REVIEWED` | reviews done, mergeability still computing | re-check shortly |
| `VALIDATING` | reviews done, CI running | step 7 — `release-core pr checks-wait` |
| `READY` | reviewed + CI green + mergeable | step 8 — flip draft→ready if drafted, page the user |
| `BLOCKED` | failing check, merge conflict, **or a circuit breaker fired** | stop; surface the reason to the user |

**Gemini is best-effort.** A silent or quota'd Gemini never holds the PR in `REVIEWS_PENDING` — only Copilot (required) gates. The snapshot is stateless and has no clock, so the *skip-after-timeout* call for a slow best-effort reviewer is yours: if you've already waited out `release-core pr copilot wait` and Gemini still shows `in_progress`, proceed.

**Circuit breakers.** When `release-core pr status` returns `BLOCKED` with a `breaker:` line (`cycle-cap`, `diff-trajectory`, `comment-set`, `repeat-finding`), the review loop is diverging — do **not** push another fixup cycle. Stop and surface the breaker reason to the user. This is the first-class "stop and hand back" outcome, not a failure.

### Leave a handoff note when you open the PR

When you open a PR, drop a short handoff note capturing the **non-obvious reasoning** behind the change — the decisions a reviewer (or a later fixer agent) couldn't re-derive from the diff: why a particular approach, what's deliberately out of scope, what *not* to "fix." Put it either in the PR body under a `## Context` heading or in `.release/handoff-<pr>.md` (use the `/handoff` skill to generate it).

Why: a detached auto-fix agent (`orc watch --auto`, release#338) addresses review comments as a **fresh** agent — it has the code but not your reasoning. The handoff note is the cheap, durable carrier of that reasoning, so the fixer respects deliberate decisions instead of undoing them to satisfy a comment. Skip it only for trivial chore/CI PRs.

### A note on draft PRs

**PR drafts are user-requested only.** Never pass `--draft` to `gh pr create` unless the user used one of these exact triggers in *this* session: "open as draft", "draft PR", "WIP PR", "draft this", "for early feedback". Absent that explicit phrase, the PR is **live**. This is a hard rule.

Common signals that should *not* trigger a draft:

- **Title patterns**: `"feature(#N): ..."`, `"PR N of M"`, `"first of stacked series"`, anything containing `skeleton`, `scaffold`, `spec`, `initial`, or `WIP`.
- **Body content**: unchecked test-plan checklist boxes, "follow-up commits planned" / "more coming" language, todo lists.
- **Vibes**: the work feeling incomplete to you. Incompleteness is signaled in the body/title; live PR status doesn't preclude WIP discussion.

Why it matters: the canonical `copilot-review.yml` workflow gates on `if: github.event.pull_request.draft == false`. Drafts silently suppress the auto-Copilot-review until the PR is manually flipped to ready, costing real time to un-draft and re-trigger the loop. The unchecked checklist / spec language / skeleton scope already communicate WIP to human reviewers; drafting on top is duplication that breaks the loop.

When genuinely uncertain: **ask before opening** — don't assume.

### A note on the `migration-in-flight` lock

Build-dir migration PRs (from `migrate-consumer-to-build-dir`) carry a `migration-in-flight` label — a lightweight signal that one agent/session already owns that consumer's migration, so a second parallel session backs off instead of opening a duplicate stacked PR (release#302). When you drive such a PR here: **leave the label alone** — it's released naturally when the PR merges/closes (a closed PR is no longer "in flight"). Don't strip it mid-loop. If you're picking up an existing migration PR that's somehow missing the label, add it (`gh pr edit <PR> --add-label migration-in-flight`) so a concurrent session sees the lock.

### Step 4: waiting for Copilot

```sh
release-core pr copilot wait <PR>
```

Run in background (`run_in_background: true`) so the conversation isn't tied up for 7+ minutes. The script will exit when Copilot's review is posted on the PR's *current head SHA* (the SHA filter is critical — `submitted_at >= start_time` was the previous design and failed when the review was already posted before the wait started).

### Step 5: triaging Copilot comments

Three categories:

**A) Project-specific real issues — address.** Examples seen:

- `cargo clippy -D warnings` → must be `cargo clippy -- -D warnings` (the `--` is required to forward `-D` to rustc).
- `permissions: { pull-requests: write }` alone removes default `contents: read`; add `contents: read` explicitly.
- Reusable workflow `uses:` refs don't follow GitHub repo redirects — use the canonical name (currently `arthur-debert/release` for copilot-review).
- Fork PRs need `github.event.pull_request.head.repo.fork == false` guard (else `requested_reviewers` POST fails).
- Pre-existing fmt drift on a repo's main shows up as "your PR breaks check" — bundle a `cargo fmt` commit into the policy PR with a clear separate-commit message.

**B) Project ethos drift — push back with rationale (don't change the file).** Reply to the comment via:

```sh
gh api 'repos/{owner}/{repo}/pulls/<PR>/comments/<COMMENT_ID>/replies' \
  -X POST -f body="..."
```

The reply ends with a line like *"Recording for future review passes: don't ask us to `<X>`"* so the rationale is searchable later. Examples seen, all pushed back on:

- **"Pin org-internal reusable workflows to a SHA."** Same owner controls both repos; supply-chain risk is negligible. Pinning defeats the "fix once, propagate" point of reusable workflows. Also baked into `copilot-instructions.md` so future passes don't re-raise.
- **"Per-repo customize the multi-repo template."** The template (`~/h/release/templates/rust/`) lists umbrella-script names that *collectively* appear across the rollout (`check`, `pre-commit`, `rust-pre-commit`, `ci.sh`); pointing at only what's local would defeat the canonical-template purpose. Contributors recognize the one their repo uses.
- **"Match the fallback Cargo flags exactly to CI's flags."** The fallback is a generic approximation; the instructions explicitly direct readers to `.github/workflows/` for the source of truth — flags vary per project.

**C) Cosmetic nits in already-merged style.** Skip. Don't reply unless the same nit is recurring — then push back generally.

### Step 5b: resolve threads as you go

After acting on each comment — fix-and-push *or* rationale reply — resolve its thread:

```sh
release-core pr resolve-thread <PR> <COMMENT_ID>
```

GitHub does not auto-resolve threads when you push a fix or reply, so without this every comment stays "Unresolved" through round 2, 3, 4 — multi-round PRs become unreadable, and there's no signal which threads are still contested versus already addressed. Resolve aggressively:

- **Fix-and-pushed → resolve.** The diff is the proof; the thread is done.
- **Rationale-replied → resolve.** Trust the agent's judgment. If Copilot disagrees on a follow-up pass, re-open or argue further.
- **Genuinely contested or awaiting human input → leave open.** That's the signal.

The end state of a healthy PR: only contested threads (and the original review summary) remain unresolved.

### Step 6: pushing fixups

Push to the same branch. CI re-runs automatically. **Do NOT** re-request Copilot on minor rounds — the workflow only auto-triggers on `opened`/`ready_for_review`, and one review per PR is the convention. Re-request only if the round of changes is substantial enough to warrant a fresh look.

Before opening *another* fixup cycle, run `release-core pr status <PR>`. If it returns `BLOCKED` with a `breaker:` line, the loop is diverging — stop, don't iterate, and surface the breaker to the user (see "Orienting with release-core pr status").

### Step 7: waiting for checks

```sh
release-core pr checks-wait <PR>
```

Run in background. Exits 0 when all checks pass, 1 if any fail.

### Step 8: stop at "ready to merge" and notify the user

`release-core pr status <PR>` returning `READY` is the signal that this point is reached (reviewed + CI green + mergeable). If the PR is a draft (e.g. a stacked feature-branch PR), flip it first so the human gate opens:

```sh
gh pr ready <PR>      # only if it was a draft
```

**Do NOT auto-merge.** The agent's job ends when the PR is in a *mergeable* state — checks green, threads resolved, CI clean. At that point, post a short status comment summarizing where things landed (or just the assistant turn — whatever's appropriate to the session) and stop. The user does the final read and merges.

```sh
# Confirm the PR is ready and stop:
gh pr view <PR> --json mergeStateStatus,mergeable --jq .
# Expected: mergeStateStatus=CLEAN, mergeable=MERGEABLE
```

**Merge only if the user has explicitly told you to.** Examples of explicit authorization:

- "go for it, merge it"
- "merge when green"
- A standing "auto-merge" instruction at the start of a batch task

When merging:

```sh
gh pr merge <PR> --squash --delete-branch
```

Always squash (rebase also works; merge commit is blocked by `required_linear_history`).

If you genuinely can't merge because of a pre-existing CI failure unrelated to the PR (e.g. the dodot CI on main has been broken since last week, and your PR's check inherited that failure), surface this clearly and ask whether to admin-bypass — don't `--admin` unprompted.

## The final-report contract (always, even mid-flow)

Whenever you end a turn on a PR — ready to merge, blocked, or stopping early — close with a structured report. This is non-negotiable when the skill runs as a subagent: the parent has no other window into what happened, and a bare *"I'll wait for the background task to complete via notifications"* forces it to re-query every PR by hand. (Half of a 20-subagent migration batch stalled exactly this way — see release#300.)

Two hard rules before you report:

1. **Re-verify actual state — don't trust the last wait result.** A `release-core pr copilot wait` / `release-core pr checks-wait` exit code can be stale by the time you stop (a check finished, a new commit landed). Always read live state first:

   ```sh
   gh pr view <PR> --json url,headRefOid,mergeStateStatus,mergeable,statusCheckRollup,reviews --jq '{url,head:.headRefOid,mergeState:.mergeStateStatus,mergeable,checks:[.statusCheckRollup[]?|{name:.name,c:.conclusion}],reviews:[.reviews[]?|{by:.author.login,state:.state}]}'
   ```

2. **Never stop with only "I'll wait for the background task."** If a background wait is genuinely needed, *poll it to completion first* — `release-core pr copilot wait` and `release-core pr checks-wait` block precisely so you can. Returning before they resolve wastes the run: the parent finds the event already arrived and has to restart you.

Then emit the report block verbatim — same shape every time so downstream agents can parse it:

```text
## Report
PR: <url>
Head SHA: <sha>
CI: <check=conclusion, ...>   (or "none yet")
Reviews: <bot/user=state, ...>   (or "none yet")
Mergeable: <yes | no — blocker>
Next step: <merge | wait for X | file issue | stop, handing back>
```

`Next step` is the actionable line — be specific (`wait for Copilot on <sha>`, not `wait`). If `Mergeable: no`, name the blocker (failing check, unresolved thread, pre-existing main breakage).

## Always update the changelog

Before pushing the PR (or at minimum before requesting review), update the `Unreleased` section. The exact location varies:

- Some repos use `CHANGELOG_UNRELEASED.md` (a separate staging file).
- Others use `## [Unreleased]` inside `CHANGELOG.md`.
- Run `ls CHANGELOG*.md` and look at git history for the convention.

For pure chore/CI PRs (no user-visible behavior change): skip and check the box `chore/docs-only` in the PR template.

## Onboarding a new repo

When the user wants to extend the standardization to a new repo:

```sh
cd <repo>
git checkout main && git pull --ff-only

# 1. GitHub-side state
release-core admin policy ruleset                              # main-branch-protection
release-core admin policy dependabot --repos <owner/repo>      # vulnerability alerts + auto-fix
pbpaste | release-core admin secrets token                    # propagate RELEASE_TOKEN (auto-discovers via ruleset)

# 2. Repo-side files
git checkout -b feat/github-policy
release-core admin policy sweep                                # rust stack only — non-rust drops in CODEOWNERS + dependabot.yml + workflows/copilot-review.yml manually until per-stack templates exist
git add .github/
git commit -m "ci: add github policy files and copilot review workflow"
git push -u origin feat/github-policy
gh pr create --title "..." --body "..."

# 3. The first PR can't auto-trigger Copilot (workflow not on main yet); request manually:
release-core pr copilot on <PR>
release-core pr copilot wait <PR>           # background

# Triage, push fixups, wait checks, merge.

# 4. Verify: audit against the new repo should be all-green or warns-only.
release-core audit --repo <owner/repo>

# 5. Smoke-test the loop end-to-end once main has the workflow:
release-core admin smoke-test <owner/repo>
```

Notes:

- The first PR is forced to use `release-core pr copilot on` manually (workflow needs to be on main first).
- Order: `release-core admin policy ruleset` → `release-core admin secrets token` → drop policy files → PR. The ruleset enables auto-discovery for `release-core admin secrets token` and `release-core admin policy dependabot`; running them before adds `--repos` overhead.
- If `release-core admin policy ruleset`'s auto-detect picks up zero checks (brand-new repo with no prior CI runs), it falls back to yq job IDs. Pass `--checks` explicitly if both fail.
- Non-rust stacks (Electron, VS Code, nvim-plugin, tree-sitter, Zed): per-stack templates aren't written yet. Drop in only the stack-agnostic 3 files (`CODEOWNERS`, `dependabot.yml`, `workflows/copilot-review.yml`) by `cp ~/h/release/templates/rust/<file>` — those have no Rust-specific content. Skip `pull_request_template.md` and `copilot-instructions.md` (rust-flavored content) until proper per-stack templates ship.

## Onboarding a stack we don't have templates for yet

Currently only `rust/` templates exist at `~/h/release/templates/`. To onboard the first electron / vsce-ext / nvim-plugin / etc. repo, first author the per-stack templates. The structure mirrors `rust/`:

```text
~/h/release/templates/<stack>/
  CODEOWNERS
  copilot-instructions.md
  dependabot.yml         # ecosystem differs per stack: cargo|npm
  pull_request_template.md
  workflows/
    copilot-review.yml   # identical across stacks
```

Then `release-core admin policy sweep` works (no dodot step needed since scripts resolve templates relative to their own location).

## Status conventions across these repos

- `mergeStateStatus: BLOCKED` usually means a required check hasn't completed yet OR the ruleset's required check names don't match the actual published check-run names. The latter is what `release-core admin policy ruleset`'s matrix-aware detection solves.
- A `Copilot Review` workflow run showing `[failure]` with no jobs visible = "workflow file issue" — usually a `uses:` reference to a non-existent workflow, or a broken redirect. Verify the reusable workflow exists at the canonical (non-redirect) name.

## When the loop misbehaves: file an issue at `arthur-debert/release`

The PR-loop infrastructure (reusable workflow, helpers, ruleset, templates) lives in `arthur-debert/release`. When the loop fails in a way the consumer repo can't fix locally, file the bug there — don't try to patch around it in the consumer.

```sh
release-core issue file <component> "<one-line symptom>"
```

Symptoms worth filing:

- `copilot-review.yml` reports SUCCESS but `requested_reviewers` is empty after 60s
- `copilot-review.yml` exits FAILURE with a non-obvious error
- `release-core pr copilot wait` times out (30m, no review) on a non-draft PR
- `rust-cli` release workflow fails in a way that looks like infra, not project code
- Ruleset blocks a merge with check names that don't exist in the workflow output
- `release-core admin policy sweep` produces a conflict that looks like a template bug

**Why:** consumers can't fix infra in place. Fix lives at `arthur-debert/release`, propagates to all consumers via `@v1`. Filing here gives one inbox for cross-repo, cross-org infra bugs and lets the user batch-triage. After the helper opens the issue, the agent should follow up with logs / suspected cause / repro steps inline as a comment.

**Don't file** for: comment nits, project-specific test failures, code-quality issues that aren't infra. Those are PR-level.

## Reference: the reusable copilot-review workflow

Lives at `arthur-debert/release/.github/workflows/copilot-review.yml@v1`. (Migrated 2026-05-08 from `arthur-debert/gh-dagentic@main`, which used `GITHUB_TOKEN` and silently no-op'd the Copilot attach across the entire portfolio for months. The smoke test caught it; a one-off sweep migrated all consumers over.) The job is named `request`. In check-run output it appears as `request / request` (caller-job / called-job format). It is *not* a required check (excluded from ruleset auto-detection by filename), so a failure of that workflow doesn't block merges — but it does mean Copilot was never requested, which is worth fixing or filing.

The workflow body uses `gh pr edit --add-reviewer @copilot` (GraphQL). It requires a user PAT with `repo + read:org` passed as `secrets.gh_token` (`RELEASE_TOKEN` is what every onboarded repo carries). Default `GITHUB_TOKEN` cannot attach Copilot — silently no-ops. Same-owner consumers can use `secrets: inherit`; cross-org consumers (lex-fmt/*) must list `gh_token: ${{ secrets.RELEASE_TOKEN }}` explicitly.

If something seems off (a smoke-test fails, Copilot isn't attaching, etc.), trust `release-core admin smoke-test <repo>` over visual inspection — the silent no-op was invisible to every other check until smoke caught it.
