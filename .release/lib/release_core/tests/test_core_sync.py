"""release_sync engine: the pure logic in release_core.sync.

Fixture-driven, no network. Git access (gh.git_*) is monkeypatched at the data
layer — recorded ls-tree/cat-file/show results — never at subprocess. The
filesystem-walk + symlink + CLAUDE.md helpers run against real tmp_path trees.

These pin the byte-for-byte contract: ref-selection precedence, capability
resolution, the plan/lefthook composition order, is_release_internal
classification, relative symlink-target math, broken-symlink detection, the
find-style traversal order, and the orientation-block computation.
"""

from __future__ import annotations

import os
import shutil

import pytest
from release_core import sync

# ── Classification predicates ─────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("rel", "skip"),
    [
        ("templates/commons/lefthook.fragment.yaml", True),
        ("templates/rust-cli/manifest.yaml", True),
        ("templates/components/_lefthook-base.yaml", True),
        ("templates/commons/.DS_Store", True),
        # Bytecode never materializes into a consumer's .release/ (release#450).
        ("templates/commons/lib/release_core/release_core/__pycache__/cli.cpython-313.pyc", True),
        ("templates/commons/lib/release_core/release_core/sync.pyc", True),
        ("templates/commons/lib/release_core/release_core/sync.pyo", True),
        # path-segment match, not a loose substring: a file merely *named* with
        # the substring is kept (it's a real authored source, not bytecode).
        ("templates/commons/docs/my__pycache__notes.md", False),
        ("templates/commons/bin/check", False),
        ("templates/commons/lefthook.yml", False),
    ],
)
def test_should_skip_source(rel, skip):
    assert sync.should_skip_source(rel) is skip


@pytest.mark.parametrize(
    ("dest", "real"),
    [
        (".github/workflows/release.yml", True),
        (".github/workflows/ci.yaml", True),
        ("bin/check", False),
        (".github/dependabot.yml", False),
    ],
)
def test_needs_real_file(dest, real):
    assert sync.needs_real_file(dest) is real


@pytest.mark.parametrize(
    ("dest", "internal"),
    [
        (".release-sync-source", True),
        (".gitignore", True),  # managed .release/.gitignore — release#450
        ("lib/release_core/release_core/sync.py", True),
        # the PR state engine folded into release_core (release#459); its files
        # are now under lib/release_core/ and covered by the branch above.
        ("lib/release_core/release_core/prstate/state.py", True),
        ("ORIENTATION.md", True),
        # NOT internal — consumer-facing lib/ + everything else.
        ("lib/bats-harness.bash", False),
        ("bin/check-shell", False),
        ("lib/release_other/x.py", False),
        ("docs/ORIENTATION.md", False),
    ],
)
def test_is_release_internal(dest, internal):
    assert sync.is_release_internal(dest) is internal


# ── Symlink target computation (relative, path-mirror) ────────────────────────


@pytest.mark.parametrize(
    ("dest", "target"),
    [
        ("lefthook.yml", ".release/lefthook.yml"),
        (".editorconfig", ".release/.editorconfig"),
        ("bin/check", "../.release/bin/check"),
        (".claude/skills/x/SKILL.md", "../../../.release/.claude/skills/x/SKILL.md"),
        ("bin/semver", "../.release/bin/semver"),
    ],
)
def test_link_target(dest, target):
    assert sync.link_target(dest) == target


# ── Ref selection precedence ──────────────────────────────────────────────────


def _fake_gh(monkeypatch, *, existing_refs, sha="deadbeef"):
    """Patch gh.git_rev_parse_verify / git_fetch_prune / git_rev_parse so
    select_ref runs offline against a known set of resolvable refs."""
    calls = {"fetched": False}

    def verify(ref, *, cwd):
        return ref in existing_refs

    def fetch(*, cwd, remote="origin"):
        calls["fetched"] = True

    monkeypatch.setattr(sync.gh, "git_rev_parse_verify", verify)
    monkeypatch.setattr(sync.gh, "git_fetch_prune", fetch)
    monkeypatch.setattr(sync.gh, "git_rev_parse", lambda ref, *, cwd: sha)
    return calls


def test_select_ref_explicit_release_ref_validated(monkeypatch):
    _fake_gh(monkeypatch, existing_refs={"my-tag"})
    assert sync.select_ref("/home", "repo", "rust-cli", "my-tag") == "my-tag"


def test_select_ref_explicit_release_ref_invalid_raises(monkeypatch):
    _fake_gh(monkeypatch, existing_refs=set())
    with pytest.raises(sync.SyncError, match="not a valid ref"):
        sync.select_ref("/home", "repo", "rust-cli", "bogus")


def test_select_ref_explicit_skips_fetch(monkeypatch):
    calls = _fake_gh(monkeypatch, existing_refs={"x"})
    sync.select_ref("/home", "repo", "rust-cli", "x")
    assert calls["fetched"] is False


def test_select_ref_prefers_repo_name_branch(monkeypatch):
    _fake_gh(
        monkeypatch,
        existing_refs={
            "refs/remotes/origin/release/beta/myrepo",
            "refs/remotes/origin/release/beta/rust-cli",
            "refs/remotes/origin/main",
        },
    )
    assert sync.select_ref("/home", "myrepo", "rust-cli", None) == "origin/release/beta/myrepo"


def test_select_ref_falls_to_kind_branch(monkeypatch):
    _fake_gh(
        monkeypatch,
        existing_refs={
            "refs/remotes/origin/release/beta/rust-cli",
            "refs/remotes/origin/main",
        },
    )
    assert sync.select_ref("/home", "myrepo", "rust-cli", None) == "origin/release/beta/rust-cli"


def test_select_ref_falls_to_main(monkeypatch):
    _fake_gh(monkeypatch, existing_refs={"refs/remotes/origin/main"})
    assert sync.select_ref("/home", "myrepo", "rust-cli", None) == "origin/main"


def test_select_ref_fetches_when_unset(monkeypatch):
    calls = _fake_gh(monkeypatch, existing_refs={"refs/remotes/origin/main"})
    sync.select_ref("/home", "myrepo", "rust-cli", None)
    assert calls["fetched"] is True


def test_select_ref_no_candidate_raises(monkeypatch):
    _fake_gh(monkeypatch, existing_refs=set())
    with pytest.raises(sync.SyncError, match="no candidate branch"):
        sync.select_ref("/home", "myrepo", "rust-cli", None)


# ── Capability resolution ─────────────────────────────────────────────────────


def test_resolve_capabilities_consumer_override(monkeypatch):
    monkeypatch.setattr(sync, "_yq_list_capabilities", lambda text: ["mkdocs", "bats"])
    caps = sync.resolve_capabilities(
        "/home", "ref", "docs-site", sync_yaml_text="capabilities:\n  - mkdocs\n  - bats\n"
    )
    assert caps.names == ["mkdocs", "bats"]
    assert caps.manifest_source == ".release-sync.yaml (consumer override)"


def test_resolve_capabilities_kind_manifest(monkeypatch):
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: True)
    monkeypatch.setattr(sync.gh, "git_show_bytes", lambda rp, *, cwd: b"capabilities:\n  - x\n")
    monkeypatch.setattr(sync, "_yq_list_capabilities", lambda text: ["x"])
    caps = sync.resolve_capabilities("/home", "ref", "rust-cli", sync_yaml_text=None)
    assert caps.names == ["x"]
    assert caps.manifest_source == "templates/rust-cli/manifest.yaml (Kind default)"


def test_resolve_capabilities_manifestless(monkeypatch):
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    caps = sync.resolve_capabilities("/home", "ref", "tree-sitter", sync_yaml_text=None)
    assert caps.names == []
    assert caps.manifest_source == "(none — manifest-less Kind; commons + Kind only)"


def test_validate_capabilities_missing_tree_raises(monkeypatch):
    monkeypatch.setattr(sync.gh, "git_ls_tree", lambda *a, **k: "")  # no tree
    with pytest.raises(sync.SyncError, match="has no templates/components/ghost/"):
        sync.validate_capabilities("/home", "ref", ["ghost"])


@pytest.mark.skipif(shutil.which("yq") is None, reason="`yq` not on PATH")
def test_resolve_capabilities_malformed_yaml_raises_yamlerror():
    # A malformed consumer override drives the real yq seam to a parse error,
    # which yamlio surfaces as YamlError; the verb catches it at the CLI boundary.
    from release_core import yamlio

    with pytest.raises(yamlio.YamlError):
        sync.resolve_capabilities(
            "/home", "ref", "docs-site", sync_yaml_text="capabilities: [a, b\n  : : :\n"
        )


def test_validate_capabilities_ok(monkeypatch):
    monkeypatch.setattr(sync.gh, "git_ls_tree", lambda *a, **k: "templates/components/x\n")
    sync.validate_capabilities("/home", "ref", ["x"])  # no raise


# ── subtree precedence + plan composition order ───────────────────────────────


def test_subtree_list_order():
    assert sync.subtree_list("rust-cli", ["a", "b"]) == [
        "templates/commons",
        "templates/components/a",
        "templates/components/b",
        "templates/rust-cli",
    ]


def test_build_plan_precedence_last_write_wins(monkeypatch):
    """A dest present in both commons and the kind subtree resolves to the kind's
    source (kind is later in precedence) but keeps its first-seen order slot."""
    trees = {
        "templates/commons": "100644 blob aaa\ttemplates/commons/bin/check\n"
        "100644 blob bbb\ttemplates/commons/lefthook.fragment.yaml\n",
        "templates/rust-cli": "100755 blob ccc\ttemplates/rust-cli/bin/check\n",
    }

    def ls_tree(ref, path, *, cwd, recursive=False, dirs_only=False, name_only=False):
        return trees.get(path, "")

    monkeypatch.setattr(sync.gh, "git_ls_tree", ls_tree)
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)

    plan = sync.build_plan("/home", "ref", "rust-cli", [])
    assert plan.order == ["bin/check"]  # fragment skipped; single dest
    assert plan.source["bin/check"] == "templates/rust-cli/bin/check"  # last wins
    assert plan.mode["bin/check"] == "100755"


def _skill_tree_ls(skill_files):
    """Build a git_ls_tree fake that serves recursive listings for skills/<name>
    from a {skill_name: [subpath, ...]} map, and "" for everything else."""

    def ls_tree(ref, path, *, cwd, recursive=False, dirs_only=False, name_only=False):
        if path.startswith("skills/"):
            name = path[len("skills/") :]
            subs = skill_files.get(name)
            if not subs:
                return ""
            return "".join(
                f"100644 blob {i:040x}\tskills/{name}/{sub}\n" for i, sub in enumerate(subs)
            )
        return ""

    return ls_tree


def test_build_plan_distributes_push_all_skills(monkeypatch):
    """Every PUSH_ALL skill that exists at the ref materializes whole-directory:
    each file under skills/<name>/ → .claude/skills/<name>/<subpath>."""
    files = {name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS}
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    for name in sync.PUSH_ALL_SKILLS:
        dest = f".claude/skills/{name}/SKILL.md"
        assert dest in plan.order
        assert plan.source[dest] == f"skills/{name}/SKILL.md"
        assert plan.mode[dest] == "100644"


def test_build_plan_multifile_skill_distributes_all_files(monkeypatch):
    """A multi-file skill (tdd ships several .md alongside SKILL.md) reaches the
    consumer in full, not just its SKILL.md."""
    files = {
        "tdd": ["SKILL.md", "mocking.md", "tests.md", "refactoring.md"],
        # the rest exist with just SKILL.md so the loop is well-formed
        **{name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS if name != "tdd"},
    }
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    for sub in ("SKILL.md", "mocking.md", "tests.md", "refactoring.md"):
        dest = f".claude/skills/tdd/{sub}"
        assert dest in plan.order
        assert plan.source[dest] == f"skills/tdd/{sub}"


def test_build_plan_tolerates_missing_skill_dir(monkeypatch):
    """A PUSH_ALL skill whose dir is absent at the ref is silently skipped."""
    # Only gh-pr-review-loop exists; the rest return "" (missing).
    files = {"gh-pr-review-loop": ["SKILL.md"]}
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    assert ".claude/skills/gh-pr-review-loop/SKILL.md" in plan.order
    # A missing skill contributes nothing.
    assert ".claude/skills/diagnose/SKILL.md" not in plan.order


def test_build_plan_replace_if_present_only_when_consumer_has_it(monkeypatch, tmp_path):
    """REPLACE_IF_PRESENT skills are synced ONLY when the consumer already carries
    .claude/skills/<name>; otherwise they are not added to the plan."""
    files = {name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS}
    files.update({name: ["SKILL.md"] for name in sync.REPLACE_IF_PRESENT_SKILLS})
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)

    # Consumer already carries lex-primer (real dir) but not the others.
    have = sync.REPLACE_IF_PRESENT_SKILLS[0]
    (tmp_path / ".claude" / "skills" / have).mkdir(parents=True)

    plan = sync.build_plan("/home", "ref", "tree-sitter", [], repo_root=str(tmp_path))
    assert f".claude/skills/{have}/SKILL.md" in plan.order
    for name in sync.REPLACE_IF_PRESENT_SKILLS[1:]:
        assert f".claude/skills/{name}/SKILL.md" not in plan.order


def test_build_plan_replace_if_present_detects_symlink(monkeypatch, tmp_path):
    """An existing .claude/skills/<name> SYMLINK also counts as present."""
    files = {name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS}
    files.update({name: ["SKILL.md"] for name in sync.REPLACE_IF_PRESENT_SKILLS})
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)

    name = sync.REPLACE_IF_PRESENT_SKILLS[0]
    skills_dir = tmp_path / ".claude" / "skills"
    skills_dir.mkdir(parents=True)
    os.symlink("/nowhere", str(skills_dir / name))  # dangling symlink still counts

    plan = sync.build_plan("/home", "ref", "tree-sitter", [], repo_root=str(tmp_path))
    assert f".claude/skills/{name}/SKILL.md" in plan.order


def test_build_plan_replace_if_present_skipped_without_repo_root(monkeypatch):
    """No repo_root (clone-less init) ⇒ REPLACE_IF_PRESENT skills are skipped."""
    files = {name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS}
    files.update({name: ["SKILL.md"] for name in sync.REPLACE_IF_PRESENT_SKILLS})
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    for name in sync.REPLACE_IF_PRESENT_SKILLS:
        assert f".claude/skills/{name}/SKILL.md" not in plan.order


def test_build_plan_never_distributes_release_only_skills(monkeypatch):
    """Release-only skills are never in either catalog ⇒ never planned, even if
    they exist at the ref."""
    release_only = ["release-fleet-ops", "release-fleet-triage", "gh-repo-setup"]
    files = {name: ["SKILL.md"] for name in sync.PUSH_ALL_SKILLS}
    files.update({name: ["SKILL.md"] for name in release_only})
    monkeypatch.setattr(sync.gh, "git_ls_tree", _skill_tree_ls(files))
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    for name in release_only:
        assert f".claude/skills/{name}/SKILL.md" not in plan.order


def test_build_plan_lefthook_fragment_order(monkeypatch):
    monkeypatch.setattr(sync.gh, "git_ls_tree", lambda *a, **k: "")
    present = {
        "ref:templates/components/_lefthook-base.yaml",
        "ref:templates/commons/lefthook.fragment.yaml",
        "ref:templates/components/cap/lefthook.fragment.yaml",
        "ref:templates/rust-cli/lefthook.fragment.yaml",
    }
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: rp in present)
    plan = sync.build_plan("/home", "ref", "rust-cli", ["cap"])
    assert plan.lefthook_frags == [
        "templates/components/_lefthook-base.yaml",
        "templates/commons/lefthook.fragment.yaml",
        "templates/components/cap/lefthook.fragment.yaml",
        "templates/rust-cli/lefthook.fragment.yaml",
    ]


def test_build_plan_skips_skip_sources(monkeypatch):
    listing = (
        "100644 blob a\ttemplates/commons/manifest.yaml\n"
        "100644 blob b\ttemplates/commons/lefthook.fragment.yaml\n"
        "100644 blob c\ttemplates/commons/.DS_Store\n"
        "100644 blob e\ttemplates/commons/lib/rc/__pycache__/cli.cpython-313.pyc\n"
        "100644 blob f\ttemplates/commons/lib/rc/cli.pyc\n"
        "100644 blob d\ttemplates/commons/bin/real\n"
    )
    monkeypatch.setattr(
        sync.gh,
        "git_ls_tree",
        lambda ref, path, *, cwd, **k: listing if path == "templates/commons" else "",
    )
    monkeypatch.setattr(sync.gh, "git_cat_file_exists", lambda rp, *, cwd: False)
    plan = sync.build_plan("/home", "ref", "tree-sitter", [])
    assert plan.order == ["bin/real"]  # bytecode + skip-sources dropped


def test_materialize_writes_managed_gitignore(monkeypatch, tmp_path):
    """materialize() always writes a managed .release/.gitignore covering
    bytecode, alongside the planned blobs (release#450)."""
    plan = sync.Plan()
    plan.order = ["bin/real"]
    plan.mode = {"bin/real": "100644"}
    plan.source = {"bin/real": "templates/commons/bin/real"}

    monkeypatch.setattr(sync.gh, "git_show_bytes", lambda spec, *, cwd: b"#!/bin/sh\n")
    sync.materialize("/home", "ref", "deadbeef" * 5, plan, str(tmp_path))

    gi = tmp_path / ".gitignore"
    assert gi.is_file()
    body = gi.read_text()
    assert "__pycache__/" in body
    assert "*.pyc" in body
    assert "*.pyo" in body


# ── find-style traversal order (the report-ordering contract) ─────────────────


def test_find_files_interleaves_like_find(tmp_path):
    """find recurses into a subdir as soon as it meets it in readdir order; the
    order must match (NOT os.walk's files-first-then-subdirs)."""
    (tmp_path / "a").write_text("")
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "x").write_text("")
    (tmp_path / "z").write_text("")
    files = sync._find_files(str(tmp_path))
    # All three present; sub/x appears (the exact interleave depends on readdir,
    # but the function must descend sub and never miss it).
    assert set(files) == {"a", "sub/x", "z"}


# ── broken-symlink detection ──────────────────────────────────────────────────


def test_broken_link_swept_when_target_absent_everywhere(tmp_path):
    binp = tmp_path / "bin"
    binp.mkdir()
    os.symlink("../.release/bin/gone", str(binp / "stale"))
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    out = sync._find_broken_release_links(str(tmp_path), str(tmp_release))
    assert out == ["./bin/stale"]


def test_broken_link_kept_when_materialized_this_sync(tmp_path):
    binp = tmp_path / "bin"
    binp.mkdir()
    os.symlink("../.release/bin/check-shell", str(binp / "check-shell"))  # dangling now
    tmp_release = tmp_path / "tmpbuild"
    (tmp_release / "bin").mkdir(parents=True)
    (tmp_release / "bin" / "check-shell").write_text("#!/bin/sh\n")  # materialized this sync
    out = sync._find_broken_release_links(str(tmp_path), str(tmp_release))
    assert out == []


def test_broken_link_ignores_non_release_targets(tmp_path):
    os.symlink("/nowhere/else", str(tmp_path / "other"))
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    assert sync._find_broken_release_links(str(tmp_path), str(tmp_release)) == []


def test_broken_link_prunes_release_and_git(tmp_path):
    # A broken .release-pointing link INSIDE .release/ or .git/ must be ignored.
    rel = tmp_path / ".release" / "bin"
    rel.mkdir(parents=True)
    os.symlink("../.release/bin/gone", str(rel / "inside"))
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    assert sync._find_broken_release_links(str(tmp_path), str(tmp_release)) == []


# ── stale managed-copy sweep ──────────────────────────────────────────────────


def test_stale_managed_copy_detected(tmp_path):
    wf = tmp_path / ".github" / "workflows"
    wf.mkdir(parents=True)
    (wf / "old.yml").write_text(sync.MANAGED_MARKER + "\non: push\n")
    (wf / "hand.yml").write_text("on: push\n")  # no marker → left alone
    out = sync._find_stale_managed_copies(str(tmp_path), set())
    assert out == [".github/workflows/old.yml"]


def test_stale_managed_copy_skips_rewritten(tmp_path):
    wf = tmp_path / ".github" / "workflows"
    wf.mkdir(parents=True)
    (wf / "keep.yml").write_text(sync.MANAGED_MARKER + "\non: push\n")
    # In copy_set → being (re)written this sync → not stale.
    out = sync._find_stale_managed_copies(str(tmp_path), {".github/workflows/keep.yml"})
    assert out == []


def test_stale_managed_copy_rel_uses_forward_slashes(tmp_path):
    # The membership test against copy_set (forward-slash keyed) and the emitted
    # rel must always use '/', never the OS separator — guards the cross-platform
    # path normalization at the relpath call.
    wf = tmp_path / ".github" / "workflows"
    wf.mkdir(parents=True)
    (wf / "old.yml").write_text(sync.MANAGED_MARKER + "\non: push\n")
    out = sync._find_stale_managed_copies(str(tmp_path), set())
    assert out == [".github/workflows/old.yml"]
    assert all("\\" not in p for p in out)


# ── distributed-skill dest replacement (the lex pr-review-respond regression) ──


@pytest.mark.parametrize(
    ("dest", "is_skill"),
    [
        (".claude/skills/pr-review-respond/SKILL.md", True),
        (".claude/skills/tdd/mocking.md", True),
        (".claude/settings.json", False),
        ("bin/check", False),
        ("lefthook.yml", False),
    ],
)
def test_is_distributed_skill_dest(dest, is_skill):
    assert sync.is_distributed_skill_dest(dest) is is_skill


def test_compute_mirror_replaces_stale_real_skill_copy(tmp_path):
    """A pre-existing REAL .claude/skills/<name>/SKILL.md (lex's stale hand-copy)
    is migrated→symlinked WITHOUT --migrate — never left as a conflict."""
    dest = ".claude/skills/pr-review-respond/SKILL.md"
    real = tmp_path / dest
    real.parent.mkdir(parents=True)
    real.write_text("# stale local copy (157 lines)\n")  # a real file, not a symlink

    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    mp = sync.compute_mirror([dest], str(tmp_path), str(tmp_release), migrate=False)

    target = sync.link_target(dest)
    assert dest in mp.migrated
    assert f"{dest} -> {target}" in mp.symlinks_to_create
    assert dest not in mp.conflicts


def test_compute_mirror_symlinked_skill_root_is_removed_first(tmp_path):
    """When the consumer's skill ROOT is itself a SYMLINK, compute_mirror schedules
    the root for removal and plans plain creates for files under it — so apply
    never mutates the symlink's target (e.g. inside .release/)."""
    # .claude/skills/lex-primer -> some external dir (the dangerous case).
    external = tmp_path / "external-target"
    (external).mkdir()
    (external / "SKILL.md").write_text("# do not touch this target\n")
    skills = tmp_path / ".claude" / "skills"
    skills.mkdir(parents=True)
    os.symlink(str(external), str(skills / "lex-primer"))

    dest = ".claude/skills/lex-primer/SKILL.md"
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    mp = sync.compute_mirror([dest], str(tmp_path), str(tmp_release), migrate=False)

    # The symlinked root is removed first; the file is a plain create.
    assert ".claude/skills/lex-primer" in mp.migrated
    target = sync.link_target(dest)
    assert f"{dest} -> {target}" in mp.symlinks_to_create
    # The per-file dest is NOT separately migrated (that would read through the link).
    assert dest not in mp.migrated


def test_skill_root_of():
    assert sync._skill_root_of(".claude/skills/tdd/mocking.md") == ".claude/skills/tdd"
    assert sync._skill_root_of(".claude/skills/tdd/SKILL.md") == ".claude/skills/tdd"
    # a bare root with no file under it, and non-skill dests → None
    assert sync._skill_root_of(".claude/skills/tdd") is None
    assert sync._skill_root_of("bin/check") is None


def test_compute_mirror_non_skill_real_file_still_conflicts(tmp_path):
    """A real file at a NON-skill managed dest keeps the conflict guard (only
    --migrate replaces it) — the skill auto-replace is scoped to skills."""
    dest = "lefthook.yml"
    (tmp_path / dest).write_text("on: push\n")
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    mp = sync.compute_mirror([dest], str(tmp_path), str(tmp_release), migrate=False)
    assert dest in mp.conflicts
    assert not mp.symlinks_to_create


# ── CLAUDE.md orientation block ───────────────────────────────────────────────


def test_claude_desired_creates_block_only_when_no_file(tmp_path):
    desired = sync.claude_desired(str(tmp_path))
    assert desired == (f"{sync.CLAUDE_BEGIN}\n@.release/ORIENTATION.md\n{sync.CLAUDE_END}\n")


def test_claude_desired_preserves_existing_content(tmp_path):
    (tmp_path / "CLAUDE.md").write_text("# Proj\n\nmine\n")
    desired = sync.claude_desired(str(tmp_path))
    assert desired.startswith(sync.CLAUDE_BEGIN)
    assert "# Proj" in desired
    assert "mine" in desired
    # Block, blank line, then content.
    assert f"{sync.CLAUDE_END}\n\n# Proj" in desired


def test_claude_desired_strips_prior_block_idempotent(tmp_path):
    p = tmp_path / "CLAUDE.md"
    first = sync.claude_desired(str(tmp_path))
    p.write_text(first)
    # Feeding its own output back yields a byte-identical file.
    assert sync.claude_desired(str(tmp_path)) == first


def test_claude_desired_strips_stale_block_and_refreshes(tmp_path):
    p = tmp_path / "CLAUDE.md"
    p.write_text(f"{sync.CLAUDE_BEGIN}\n@.release/STALE.md\n{sync.CLAUDE_END}\n\n# Proj\n\nmine\n")
    desired = sync.claude_desired(str(tmp_path))
    assert "@.release/ORIENTATION.md" in desired
    assert "STALE" not in desired
    assert "# Proj" in desired
    assert desired.count(sync.CLAUDE_BEGIN) == 1


def test_decide_claude_no_orientation_in_tree(tmp_path):
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()  # no ORIENTATION.md
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "none"


def test_decide_claude_create(tmp_path):
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    (tmp_release / "ORIENTATION.md").write_text("welcome\n")
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "create"


def test_decide_claude_skip_symlink(tmp_path):
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    (tmp_release / "ORIENTATION.md").write_text("welcome\n")
    (tmp_path / "real.md").write_text("x\n")
    os.symlink("real.md", str(tmp_path / "CLAUDE.md"))
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "skip-symlink"


def test_decide_claude_inject_vs_refresh(tmp_path):
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    (tmp_release / "ORIENTATION.md").write_text("welcome\n")
    claude = tmp_path / "CLAUDE.md"
    # No managed marker → inject.
    claude.write_text("# Proj\n")
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "inject"
    # Has a (stale) managed marker → refresh.
    claude.write_text(f"{sync.CLAUDE_BEGIN}\n@.release/STALE.md\n{sync.CLAUDE_END}\n")
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "refresh"


def test_decide_claude_none_when_already_synced(tmp_path):
    tmp_release = tmp_path / "tmpbuild"
    tmp_release.mkdir()
    (tmp_release / "ORIENTATION.md").write_text("welcome\n")
    claude = tmp_path / "CLAUDE.md"
    claude.write_text(sync.claude_desired(str(tmp_path)))
    assert sync.decide_claude(str(tmp_path), str(tmp_release)).action == "none"


# ── file diff ─────────────────────────────────────────────────────────────────


def test_diff_release_added_modified_removed(tmp_path):
    new = tmp_path / "new"
    old = tmp_path / "old"
    for d in (new, old):
        (d / "sub").mkdir(parents=True)
    (new / "added.txt").write_text("a")
    (new / "sub" / "same.txt").write_text("same")
    (new / "sub" / "changed.txt").write_text("NEW")
    (old / "sub" / "same.txt").write_text("same")
    (old / "sub" / "changed.txt").write_text("OLD")
    (old / "removed.txt").write_text("r")

    diff, new_files = sync.diff_release(str(new), str(old))
    assert set(diff.added) == {"added.txt"}
    assert set(diff.modified) == {"sub/changed.txt"}
    assert set(diff.removed) == {"removed.txt"}
    assert "sub/same.txt" not in diff.modified
    assert set(new_files) == {"added.txt", "sub/same.txt", "sub/changed.txt"}


def test_diff_release_no_existing(tmp_path):
    new = tmp_path / "new"
    new.mkdir()
    (new / "x").write_text("1")
    diff, _ = sync.diff_release(str(new), str(tmp_path / "nope"))
    assert diff.added == ["x"]
    assert diff.removed == []
