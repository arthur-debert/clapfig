"""sync — the release-sync engine (build-dir + symlinks, ADR-0001).

Pure(-ish) port of bin/release-sync: ref selection, Kind+Capability resolution,
the materialize-into-a-fresh-.release/ plan, lefthook fragment composition,
release-internal classification, symlink-target computation, the diff against an
existing .release/, broken-symlink detection, and the CLAUDE.md orientation
block. The verb (verbs/release_sync.py) wires these together with the CLI guards
and the apply phase.

All git access goes through gh.py (the chokepoint). Filesystem reads/writes use
the stdlib. Behavior mirrors the bash byte-for-byte — see the per-function notes
that pin each bash construct.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass, field

from . import gh

# ── Constants (mirror the bash globals verbatim) ──────────────────────────────

MANAGED_MARKER = "# Managed by release-sync — do not edit. Regenerate via release-sync."
SOURCE_MARKER = ".release-sync-source"

CLAUDE_FILE = "CLAUDE.md"
CLAUDE_BEGIN = "<!-- BEGIN release-managed orientation — managed by release-sync; do not edit -->"
CLAUDE_END = "<!-- END release-managed orientation -->"

PR_LOOP_SKILL_SRC = "skills/gh-pr-review-loop/SKILL.md"
PR_LOOP_SKILL_DEST = ".claude/skills/gh-pr-review-loop/SKILL.md"


# ── Classification predicates (the bash case statements) ──────────────────────


def should_skip_source(rel: str) -> bool:
    """Mirror should_skip_source(): drop lefthook.fragment.yaml, manifest.yaml,
    templates/components/_*, and *.DS_Store."""
    if rel.endswith("/lefthook.fragment.yaml"):
        return True
    if rel.endswith("/manifest.yaml"):
        return True
    if rel.startswith("templates/components/_"):
        return True
    return rel.endswith(".DS_Store")


def needs_real_file(dest: str) -> bool:
    """Mirror needs_real_file(): .github/workflows/* are written as real copies
    (GH reads workflow YAML from the tree and won't dereference a symlink)."""
    return dest.startswith(".github/workflows/")


def is_release_internal(dest: str) -> bool:
    """Mirror is_release_internal(): content materialized into .release/ but NOT
    mirrored out as a symlink/copy. The provenance marker, the Python engine
    packages (lib/release_gh/*, lib/release_core/*), and ORIENTATION.md."""
    if dest == SOURCE_MARKER:
        return True
    if dest.startswith("lib/release_gh/"):
        return True
    if dest.startswith("lib/release_core/"):
        return True
    return dest == "ORIENTATION.md"


# ── Ref selection ─────────────────────────────────────────────────────────────


class SyncError(RuntimeError):
    """A fatal release-sync condition (maps to exit 1)."""


def select_ref(release_home: str, repo_name: str, kind: str, release_ref: str | None) -> str:
    """First-match-wins ref selection (mirrors the bash `--- Ref selection ---`).

    $RELEASE_REF (validated) → origin/release/beta/<repo-name> →
    origin/release/beta/<kind> → main. Fetches origin --prune only when
    RELEASE_REF is unset. Raises SyncError on a bad RELEASE_REF or no candidate.
    """
    if release_ref:
        if not gh.git_rev_parse_verify(release_ref, cwd=release_home):
            raise SyncError(f"release-sync: $RELEASE_REF='{release_ref}' is not a valid ref")
        return release_ref

    gh.git_fetch_prune(cwd=release_home)
    for candidate in (f"release/beta/{repo_name}", f"release/beta/{kind}", "main"):
        if gh.git_rev_parse_verify(f"refs/remotes/origin/{candidate}", cwd=release_home):
            return f"origin/{candidate}"
    raise SyncError("release-sync: no candidate branch found in $RELEASE_HOME")


# ── Capability resolution ─────────────────────────────────────────────────────


@dataclass
class Capabilities:
    names: list[str]
    manifest_source: str


def _yq_list_capabilities(text: str) -> list[str]:
    """Mirror `yq '.capabilities // [] | .[]'` over a YAML document: one element
    per line. Done via yamlio so the YAML seam stays single-sourced."""
    from . import yamlio

    data = yamlio.loads(text)
    if not isinstance(data, dict):
        return []
    caps = data.get("capabilities")
    if not isinstance(caps, list):
        return []
    # yq prints each scalar on its own line; mapfile splits on newlines. A
    # non-scalar element would render oddly, but capabilities are always scalars.
    return [str(c) for c in caps]


def resolve_capabilities(
    release_home: str,
    ref: str,
    kind: str,
    *,
    sync_yaml_text: str | None,
) -> Capabilities:
    """Mirror the `--- Capability resolution ---` block.

    Precedence: a consumer .release-sync.yaml (its text passed in) overrides the
    Kind manifest; a manifest-less Kind yields no capabilities. Returns the
    declared names + the human-readable manifest_source label.
    """
    if sync_yaml_text is not None:
        return Capabilities(
            names=_yq_list_capabilities(sync_yaml_text),
            manifest_source=".release-sync.yaml (consumer override)",
        )
    if gh.git_cat_file_exists(f"{ref}:templates/{kind}/manifest.yaml", cwd=release_home):
        text = gh.git_show_bytes(f"{ref}:templates/{kind}/manifest.yaml", cwd=release_home).decode(
            "utf-8", "replace"
        )
        return Capabilities(
            names=_yq_list_capabilities(text),
            manifest_source=f"templates/{kind}/manifest.yaml (Kind default)",
        )
    return Capabilities(
        names=[],
        manifest_source="(none — manifest-less Kind; commons + Kind only)",
    )


def validate_capabilities(release_home: str, ref: str, capabilities: list[str]) -> None:
    """Mirror the per-capability `ls-tree -d` existence guard."""
    for c in capabilities:
        if not c:
            continue
        listing = gh.git_ls_tree(
            ref, f"templates/components/{c}", cwd=release_home, dirs_only=True, name_only=True
        )
        if not _has_nonempty_line(listing):
            raise SyncError(
                f"release-sync: declared Capability '{c}' has no "
                f"templates/components/{c}/ tree in {ref}"
            )


def _has_nonempty_line(text: str) -> bool:
    """Mirror `| grep -q .` — true iff any line has at least one character."""
    return any(line for line in text.splitlines())


# ── Plan: source path → (mode, dest-relative-to-.release/) ────────────────────


@dataclass
class Plan:
    """The materialization plan. ``order`` preserves first-seen dest order
    (mirrors plan_order); ``mode``/``source`` map dest → git filemode / source
    path (last write wins, mirroring the bash assoc-array overwrite)."""

    order: list[str] = field(default_factory=list)
    mode: dict[str, str] = field(default_factory=dict)
    source: dict[str, str] = field(default_factory=dict)
    lefthook_frags: list[str] = field(default_factory=list)


def subtree_list(kind: str, capabilities: list[str]) -> list[str]:
    """Mirror the subtrees array: commons < each capability < kind."""
    subtrees = ["templates/commons"]
    for c in capabilities:
        if c:
            subtrees.append(f"templates/components/{c}")
    subtrees.append(f"templates/{kind}")
    return subtrees


def build_plan(release_home: str, ref: str, kind: str, capabilities: list[str]) -> Plan:
    """Mirror the `--- Plan ---` block: walk each subtree with ls-tree -r, skip
    the should_skip_source paths, strip the subtree prefix to get the dest, then
    add the PR-loop skill and compose the lefthook fragment list."""
    plan = Plan()

    for st in subtree_list(kind, capabilities):
        listing = gh.git_ls_tree(ref, st, cwd=release_home, recursive=True)
        for line in listing.splitlines():
            if not line:
                continue
            # ls-tree -r line: "<mode> <type> <sha>\t<path>". meta is the part
            # before the tab; file_mode is its first whitespace field.
            meta, tab, rel = line.partition("\t")
            if not tab or not rel:
                continue
            if should_skip_source(rel):
                continue
            file_mode = meta.split(" ", 1)[0]
            # dest = ${rel#"$st"/}
            prefix = st + "/"
            dest = rel[len(prefix) :] if rel.startswith(prefix) else rel
            if dest not in plan.source:
                plan.order.append(dest)
            plan.mode[dest] = file_mode
            plan.source[dest] = rel

    # #348 A3: distribute the canonical PR-loop skill, sourced directly from skills/.
    if gh.git_cat_file_exists(f"{ref}:{PR_LOOP_SKILL_SRC}", cwd=release_home):
        if PR_LOOP_SKILL_DEST not in plan.source:
            plan.order.append(PR_LOOP_SKILL_DEST)
        plan.mode[PR_LOOP_SKILL_DEST] = "100644"
        plan.source[PR_LOOP_SKILL_DEST] = PR_LOOP_SKILL_SRC

    # Compose lefthook.yml fragment list: base < commons < each capability < kind.
    frags: list[str] = []
    base = "templates/components/_lefthook-base.yaml"
    if gh.git_cat_file_exists(f"{ref}:{base}", cwd=release_home):
        frags.append(base)
    commons = "templates/commons/lefthook.fragment.yaml"
    if gh.git_cat_file_exists(f"{ref}:{commons}", cwd=release_home):
        frags.append(commons)
    for c in capabilities:
        if not c:
            continue
        fpath = f"templates/components/{c}/lefthook.fragment.yaml"
        if gh.git_cat_file_exists(f"{ref}:{fpath}", cwd=release_home):
            frags.append(fpath)
    stack_frag = f"templates/{kind}/lefthook.fragment.yaml"
    if gh.git_cat_file_exists(f"{ref}:{stack_frag}", cwd=release_home):
        frags.append(stack_frag)
    plan.lefthook_frags = frags

    return plan


# ── Symlink target computation ────────────────────────────────────────────────


def link_target(dest: str) -> str:
    """Mirror the relative-symlink target computation.

    For a top-level dest the target is `.release/<dest>`; otherwise prefix one
    `../` per path component of the dest's directory."""
    link_dir = os.path.dirname(dest)
    if link_dir in ("", "."):
        return f".release/{dest}"
    # depth = (number of '/' in link_dir) + 1  — bash: tr -cd '/' | wc -c, +1.
    depth = link_dir.count("/") + 1
    prefix = "../" * depth
    return f"{prefix}.release/{dest}"


# ── Materialize the new tree into a tempdir ───────────────────────────────────


def materialize(release_home: str, ref: str, ref_sha: str, plan: Plan, tmp_release: str) -> None:
    """Mirror `--- Build the new .release/ tree in a tempdir ---`: write each
    planned blob (preserving the 100755/100644 mode), the composed lefthook.yml,
    and the provenance marker into ``tmp_release``."""
    for dest in plan.order:
        src = plan.source[dest]
        fmode = plan.mode[dest]
        out_path = os.path.join(tmp_release, dest)
        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        content = gh.git_show_bytes(f"{ref}:{src}", cwd=release_home)
        with open(out_path, "wb") as fh:
            fh.write(content)
        os.chmod(out_path, 0o755 if fmode == "100755" else 0o644)

    if plan.lefthook_frags:
        _write_lefthook(release_home, ref, ref_sha, plan.lefthook_frags, tmp_release)

    # Provenance marker (ADR-0002): static comment lines + the full source SHA.
    marker = os.path.join(tmp_release, SOURCE_MARKER)
    with open(marker, "w", encoding="utf-8") as fh:
        fh.write(
            "# release-sync provenance — the arthur-debert/release commit that\n"
            "# generated this .release/. Informational, not operational state (ADR-0002).\n"
            "# Regenerated on every sync. release-drift-check reads the SHA below.\n"
            f"{ref_sha}\n"
        )


def _write_lefthook(
    release_home: str, ref: str, ref_sha: str, frags: list[str], tmp_release: str
) -> None:
    """Mirror the lefthook.yml generation: materialize each fragment to a
    NN-<dir>.yaml temp file (the numeric prefix fixes the merge order), then
    `yq eval-all '. as $i ireduce({}; . *+ $i) | ... comments=""'` over them,
    under the generated-by header. The `*+` deep-merges with array concat; the
    comment-strip drops fragment comments."""
    import shutil
    import tempfile

    from . import yamlio

    frag_tmp = tempfile.mkdtemp()
    try:
        frag_files: list[str] = []
        for i, fp in enumerate(frags):
            dirbase = os.path.basename(os.path.dirname(fp))
            out_path = os.path.join(frag_tmp, f"{i:02d}-{dirbase}.yaml")
            content = gh.git_show_bytes(f"{ref}:{fp}", cwd=release_home)
            with open(out_path, "wb") as fh:
                fh.write(content)
            frag_files.append(out_path)

        merged = yamlio.eval_all('. as $i ireduce({}; . *+ $i) | ... comments=""', frag_files)
        header = (
            f"# Generated by release-sync from arthur-debert/release@{ref_sha[:12]}. Do not edit.\n"
            "# Regenerate by running release-sync.\n\n"
        )
        lefthook_out = os.path.join(tmp_release, "lefthook.yml")
        with open(lefthook_out, "w", encoding="utf-8") as fh:
            fh.write(header)
            fh.write(merged)
    finally:
        shutil.rmtree(frag_tmp, ignore_errors=True)


# ── Diff: new tree vs existing .release/ ──────────────────────────────────────


@dataclass
class FileDiff:
    added: list[str] = field(default_factory=list)
    modified: list[str] = field(default_factory=list)
    removed: list[str] = field(default_factory=list)


def _find_files(root: str) -> list[str]:
    """All regular-file paths under ``root``, relative to it, in `find -type f`
    traversal order.

    Crucially this mirrors GNU/BSD `find`, NOT os.walk: find interleaves files
    and dirs in readdir order, recursing into a subdir AS SOON as it encounters
    it — whereas os.walk yields all of a directory's files first, then recurses.
    The two diverge whenever a directory holds files both before and after a
    subdir entry, which shifts the report's +file ordering. Matching find keeps
    the report byte-for-byte with the bash on the same filesystem."""
    out: list[str] = []

    def walk(d: str, rel: str) -> None:
        try:
            entries = list(os.scandir(d))
        except OSError:
            return
        for entry in entries:  # readdir order, exactly as find consumes it
            child_rel = f"{rel}/{entry.name}" if rel else entry.name
            if entry.is_dir(follow_symlinks=False):
                walk(entry.path, child_rel)
            elif entry.is_file(follow_symlinks=False):
                out.append(child_rel)

    walk(root, "")
    return out


def diff_release(tmp_release: str, existing_release: str) -> tuple[FileDiff, list[str]]:
    """Mirror `--- Compute changes ---`: added/modified/removed of files in
    .release/ comparing the new tree to the existing one. Returns the FileDiff
    plus the ordered list of new-tree relative paths (used downstream)."""
    new_files = _find_files(tmp_release)
    old_files = _find_files(existing_release) if os.path.isdir(existing_release) else []
    new_set = set(new_files)
    old_set = set(old_files)

    diff = FileDiff()
    for f in new_files:
        if f not in old_set:
            diff.added.append(f)
        elif not _files_equal(os.path.join(tmp_release, f), os.path.join(existing_release, f)):
            diff.modified.append(f)
    for f in old_files:
        if f not in new_set:
            diff.removed.append(f)
    return diff, new_files


def _files_equal(a: str, b: str) -> bool:
    """Mirror `cmp -s` — byte-identical comparison."""
    try:
        with open(a, "rb") as fa, open(b, "rb") as fb:
            return fa.read() == fb.read()
    except OSError:
        return False


# ── Symlink / copy / conflict plan against the consumer working tree ──────────


@dataclass
class MirrorPlan:
    symlinks_to_create: list[str] = field(default_factory=list)  # "f -> target"
    symlinks_to_remove: list[str] = field(default_factory=list)
    copies_to_write: list[str] = field(default_factory=list)
    copies_to_remove: list[str] = field(default_factory=list)
    conflicts: list[str] = field(default_factory=list)
    migrated: list[str] = field(default_factory=list)


def compute_mirror(
    new_files: list[str], repo_root: str, tmp_release: str, *, migrate: bool
) -> MirrorPlan:
    """Mirror the symlink/copy/conflict planning loop + the broken-symlink sweep
    + the stale managed-copy sweep. ``repo_root`` is the consumer cwd; paths are
    relative to it (the bash ran after `cd "$repo_root"`)."""
    mp = MirrorPlan()

    for f in new_files:
        if is_release_internal(f):
            continue
        if needs_real_file(f):
            mp.copies_to_write.append(f)
            continue

        target = link_target(f)
        abs_f = os.path.join(repo_root, f)
        if os.path.islink(abs_f):
            current = os.readlink(abs_f)
            if current != target:
                mp.symlinks_to_create.append(f"{f} -> {target}")
        elif os.path.lexists(abs_f):
            # exists (non-symlink): a real file at the managed location.
            if migrate:
                mp.migrated.append(f)
                mp.symlinks_to_create.append(f"{f} -> {target}")
            else:
                mp.conflicts.append(f)
        else:
            mp.symlinks_to_create.append(f"{f} -> {target}")

    mp.symlinks_to_remove = _find_broken_release_links(repo_root, tmp_release)
    mp.copies_to_remove = _find_stale_managed_copies(repo_root, set(mp.copies_to_write))
    return mp


def _find_broken_release_links(repo_root: str, tmp_release: str) -> list[str]:
    """Mirror the broken-symlink sweep: walk the repo (excluding .git/ and
    .release/) for symlinks whose target contains `.release/`; a link is stale
    only if its target is absent from BOTH the live tree AND the new tree.

    Paths are returned relative to repo_root, prefixed `./` exactly as the bash
    `find . -type l` emitted them (e.g. './bin/stale-tool'), in find traversal
    order (readdir, recursing into a real dir as it is encountered; .git and
    .release pruned; a symlinked dir is `-type l` so find does not descend it)."""
    out: list[str] = []

    def walk(d: str, rel: str) -> None:
        try:
            entries = list(os.scandir(d))
        except OSError:
            return
        for entry in entries:
            child_rel = f"{rel}/{entry.name}" if rel else f"./{entry.name}"
            if entry.is_symlink():
                target = os.readlink(entry.path)
                if ".release/" not in target:
                    continue
                # rel-after-marker = "${target##*.release/}" (text after the LAST).
                tgt_rel = target.rsplit(".release/", 1)[1]
                # broken (target absent live) AND not materialized this sync.
                if not os.path.exists(entry.path) and not os.path.exists(
                    os.path.join(tmp_release, tgt_rel)
                ):
                    out.append(child_rel)
            elif entry.is_dir(follow_symlinks=False):
                # Prune .git and .release at the top level (find -not -path).
                if rel == "" and entry.name in (".git", ".release"):
                    continue
                walk(entry.path, child_rel)

    walk(repo_root, "")
    return out


def _find_stale_managed_copies(repo_root: str, copy_set: set[str]) -> list[str]:
    """Mirror the stale managed-copy sweep under .github/workflows/: real files
    carrying the MANAGED_MARKER header that are not being (re)written this sync."""
    out: list[str] = []
    wf_dir = os.path.join(repo_root, ".github/workflows")
    if not os.path.isdir(wf_dir):
        return out
    for dirpath, _dirnames, filenames in os.walk(wf_dir):
        for name in filenames:
            full = os.path.join(dirpath, name)
            # copy_set is keyed with forward slashes (git/POSIX paths); force the
            # separator so the membership test holds on every platform. On the
            # supported macOS/Linux runners os.sep is already "/", so this is a
            # no-op there and purely defensive for a hypothetical Windows host.
            rel = os.path.relpath(full, repo_root).replace(os.sep, "/")
            if rel in copy_set:
                continue
            if os.path.islink(full):
                continue
            if _first_line_has_marker(full):
                out.append(rel)
    return out


def _first_line_has_marker(path: str) -> bool:
    """Mirror `head -1 <f> | grep -qF "$MANAGED_MARKER"`."""
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            first = fh.readline()
    except OSError:
        return False
    return MANAGED_MARKER in first


# ── CLAUDE.md orientation block (#348) ────────────────────────────────────────


def claude_desired(repo_root: str) -> str:
    """Mirror claude_desired(): the managed block at the top, then the consumer's
    existing content (with any prior managed block stripped, leading blanks
    trimmed) below it. Reads $CLAUDE_FILE; returns the candidate content."""
    rest = ""
    claude_path = os.path.join(repo_root, CLAUDE_FILE)
    if os.path.isfile(claude_path) and not os.path.islink(claude_path):
        rest = _strip_managed_block(claude_path)

    out = f"{CLAUDE_BEGIN}\n@.release/ORIENTATION.md\n{CLAUDE_END}\n"
    if rest:
        out += f"\n{rest}\n"
    return out


def _strip_managed_block(path: str) -> str:
    """Mirror the awk(strip BEGIN..END block) | sed('/./,$!d')(drop leading blank
    lines). The awk skips lines from the one containing BEGIN through the one
    containing END (inclusive), printing the rest; sed then deletes leading blank
    lines. Returns the result WITHOUT a trailing newline (matches `$(...)`)."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().split("\n")
    # The file content split on '\n'; a trailing newline yields a final ''.
    kept: list[str] = []
    skip = False
    for line in lines:
        if CLAUDE_BEGIN in line:
            skip = True
        if not skip:
            kept.append(line)
        if CLAUDE_END in line:
            skip = False
    # sed '/./,$!d' deletes leading blank lines (until the first non-blank).
    while kept and kept[0] == "":
        kept.pop(0)
    # The command substitution $(...) strips trailing newlines; rejoin then strip.
    return "\n".join(kept).rstrip("\n")


@dataclass
class ClaudeDecision:
    action: str  # none | create | inject | refresh | skip-symlink
    desired: str | None = None  # the candidate content (None for none/skip)


def decide_claude(repo_root: str, tmp_release: str) -> ClaudeDecision:
    """Mirror the `--- CLAUDE.md orientation block ---` decision. Only acts when
    the synced tree carries ORIENTATION.md (always, via commons)."""
    if not os.path.isfile(os.path.join(tmp_release, "ORIENTATION.md")):
        return ClaudeDecision("none")

    claude_path = os.path.join(repo_root, CLAUDE_FILE)
    if os.path.islink(claude_path):
        return ClaudeDecision("skip-symlink")

    desired = claude_desired(repo_root)
    if not os.path.lexists(claude_path):
        return ClaudeDecision("create", desired)
    existing = _read_text(claude_path)
    if existing == desired:
        return ClaudeDecision("none", desired)
    if CLAUDE_BEGIN in existing:
        return ClaudeDecision("refresh", desired)
    return ClaudeDecision("inject", desired)


def _read_text(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


# A few constants other modules / tests may want.
_LEFTHOOK_HEADER_RE = re.compile(r"^# Generated by release-sync")
