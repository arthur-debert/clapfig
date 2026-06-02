"""init verb (verbs/init.py): the create-if-absent config materializer that is
the pip-bootstrap PoC seam (§2).

The release-sync engine that composes the source content is exercised by
test_core_sync.py / test_core_release_sync_verb.py; here we monkeypatch
init._materialize_config_sources to hand back a temp tree of fixture config
files, then pin init's OWN contract: create-if-absent, idempotency, --force
overwrite, --dry-run, and the hard non-zero exit when a write fails. The source
resolution + Kind/ref failure surfaces are covered by their own tests below.
"""

from __future__ import annotations

import os
import stat

from release_core import manifest, sync, yamlio
from release_core.verbs import init


def _fixture_sources(tmp_path) -> dict[str, str]:
    """A temp tree with one fixture file per CONFIG_FILES dest; return the
    {dest -> abs path} map init._materialize_config_sources would return."""
    src_root = tmp_path / "src"
    src_root.mkdir()
    sources: dict[str, str] = {}
    for dest in init.CONFIG_FILES:
        p = src_root / dest
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(f"# managed source for {dest}\n")
        sources[dest] = str(p)
    return sources


def _patch(monkeypatch, repo, sources):
    """Wire init's repo-root + source resolution to the fixture repo/sources."""
    monkeypatch.setattr(init.gh, "repo_root", lambda: str(repo))
    monkeypatch.setattr(init, "_materialize_config_sources", lambda root, name: sources)


# --------------------------------------------------------------------------
# create-if-absent
# --------------------------------------------------------------------------


def test_init_creates_all_config_files_when_absent(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    rc = init.main([])
    out = capsys.readouterr().out
    assert rc == 0
    for dest in init.CONFIG_FILES:
        target = repo / dest
        assert target.is_file(), f"{dest} should have been created"
        assert target.read_text() == f"# managed source for {dest}\n"
        assert f"create  {dest}" in out
    assert "7 created, 0 overwritten, 0 unchanged" in out
    assert "done." in out


def test_init_leaves_existing_files_untouched(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    # A pre-existing consumer edit must survive (create-if-absent, no --force).
    (repo / "lefthook.yml").write_text("# CONSUMER EDIT — keep me\n")
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    rc = init.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert (repo / "lefthook.yml").read_text() == "# CONSUMER EDIT — keep me\n"
    assert "skip    lefthook.yml" in out
    # The other six were absent → created.
    assert "6 created, 0 overwritten, 1 unchanged" in out


# --------------------------------------------------------------------------
# idempotency — second run is a clean no-op
# --------------------------------------------------------------------------


def test_init_is_idempotent_second_run_is_clean_no_op(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    assert init.main([]) == 0
    capsys.readouterr()
    # Snapshot the tree after the first run.
    before = {
        dest: (repo / dest).read_text() for dest in init.CONFIG_FILES if (repo / dest).is_file()
    }

    rc = init.main([])
    out = capsys.readouterr().out
    assert rc == 0
    after = {
        dest: (repo / dest).read_text() for dest in init.CONFIG_FILES if (repo / dest).is_file()
    }
    assert after == before, "second run must not change any file"
    assert "0 created, 0 overwritten, 7 unchanged" in out
    assert "no changes — already initialized" in out


# --------------------------------------------------------------------------
# --force overwrite
# --------------------------------------------------------------------------


def test_init_force_overwrites_existing(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "lefthook.yml").write_text("# stale\n")
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    rc = init.main(["--force"])
    out = capsys.readouterr().out
    assert rc == 0
    assert (repo / "lefthook.yml").read_text() == "# managed source for lefthook.yml\n"
    assert "force   lefthook.yml (overwritten)" in out
    # 6 absent → created, 1 present → overwritten.
    assert "6 created, 1 overwritten, 0 unchanged" in out


def test_init_force_preserves_existing_file_mode(tmp_path, monkeypatch, capsys):
    # The atomic overwrite goes through mkstemp (0600) + os.replace; it must NOT
    # silently tighten the managed file's permissions. (Gemini review on #424.)
    repo = tmp_path / "repo"
    repo.mkdir()
    target = repo / "lefthook.yml"
    target.write_text("# stale\n")
    os.chmod(target, 0o644)
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    assert init.main(["--force"]) == 0
    capsys.readouterr()
    mode = stat.S_IMODE(os.stat(target).st_mode)
    assert mode == 0o644, f"force overwrite changed mode to {oct(mode)} (expected 0o644)"


def test_init_repairs_a_broken_symlink(tmp_path, monkeypatch, capsys):
    # A dangling .release/-style symlink at a config path reports as present via
    # lexists(); init must repair it (materialize the real file over it) even
    # WITHOUT --force, not silently skip and leave the repo uninitialized.
    # (Gemini review on #424.)
    repo = tmp_path / "repo"
    repo.mkdir()
    link = repo / "lefthook.yml"
    os.symlink(repo / ".release" / "build" / "lefthook.yml", link)  # target missing
    assert os.path.islink(link) and not os.path.exists(link)
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    rc = init.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert not os.path.islink(link), "broken symlink should be replaced by a real file"
    assert link.read_text() == "# managed source for lefthook.yml\n"
    assert "repair  lefthook.yml (was a broken symlink)" in out
    assert "1 repaired" in out


def test_init_resolves_relative_release_home_before_chdir(tmp_path, monkeypatch):
    # A relative RELEASE_HOME must be resolved against the ORIGINAL cwd, not the
    # repo root init chdir's into. (Gemini review on #424.)
    monkeypatch.chdir(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)
    monkeypatch.setenv("RELEASE_HOME", "rel/clone")

    assert init.main([]) == 0
    assert os.environ["RELEASE_HOME"] == str(tmp_path / "rel" / "clone")


# --------------------------------------------------------------------------
# --dry-run writes nothing
# --------------------------------------------------------------------------


def test_init_dry_run_writes_nothing(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    rc = init.main(["--dry-run"])
    out = capsys.readouterr().out
    assert rc == 0
    for dest in init.CONFIG_FILES:
        assert not (repo / dest).exists(), f"{dest} must NOT be written in dry-run"
        assert f"would create  {dest}" in out
    assert "dry-run, no writes" in out


# --------------------------------------------------------------------------
# hard non-zero exit when a write fails
# --------------------------------------------------------------------------


def test_init_returns_1_when_a_write_fails(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    _patch(monkeypatch, repo, sources)

    # Make the very first managed write raise OSError — init must hard-fail
    # (exit 1, clean stderr), never silently best-effort past it.
    def boom(dest, src, *, exists):
        raise OSError(13, "Permission denied")

    monkeypatch.setattr(init, "_write_file", boom)

    rc = init.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "failed to write" in err


# --------------------------------------------------------------------------
# config-file list provenance — the documented seam scope
# --------------------------------------------------------------------------


def test_config_files_is_the_documented_config_subset():
    # The exact list PR-C chose from sync.py (lefthook.yml + the managed lint/
    # format configs). Pinned so a future drift is a conscious edit, and so the
    # PR-body provenance claim stays honest.
    assert init.CONFIG_FILES == (
        "lefthook.yml",
        ".markdownlint.json",
        ".markdownlintignore",
        ".yamllint",
        ".shellcheckrc",
        ".editorconfig",
        ".prettierignore",
    )
    # None of the release-internal / package-code paths leak in.
    for dest in init.CONFIG_FILES:
        assert not dest.startswith("lib/"), "package code is NOT init's scope"
        assert dest != sync.SOURCE_MARKER
        assert dest != "ORIENTATION.md"


# --------------------------------------------------------------------------
# resolution-failure surfaces (Kind / ref) → exit 1, clean message
# --------------------------------------------------------------------------


def test_init_kind_error_exits_1(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(init.gh, "repo_root", lambda: str(repo))

    def raise_kind(root, name):
        raise manifest.KindError("nope")

    monkeypatch.setattr(init, "_materialize_config_sources", raise_kind)
    rc = init.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "could not detect kind" in err


def test_init_sync_error_exits_1(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(init.gh, "repo_root", lambda: str(repo))

    def raise_sync(root, name):
        raise sync.SyncError("release-core init: $RELEASE_HOME=... is not a git clone")

    monkeypatch.setattr(init, "_materialize_config_sources", raise_sync)
    rc = init.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "is not a git clone" in err


def test_init_not_in_git_repo_exits_1(monkeypatch, capsys):
    def boom():
        raise RuntimeError("not a git repo")

    monkeypatch.setattr(init.gh, "repo_root", boom)
    rc = init.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "not inside a git repo" in err


def test_init_missing_source_for_kind_is_reported_not_fatal(tmp_path, monkeypatch, capsys):
    # If the engine produced no lefthook.yml (a Kind whose gate composes none),
    # init reports it on stderr and still materializes the rest, exit 0 — but the
    # final line must NOT claim the repo is fully initialized.
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = _fixture_sources(tmp_path)
    del sources["lefthook.yml"]
    _patch(monkeypatch, repo, sources)

    rc = init.main([])
    captured = capsys.readouterr()
    assert rc == 0
    assert "absent  lefthook.yml" in captured.err
    assert not (repo / "lefthook.yml").exists()
    assert (repo / ".yamllint").is_file()
    # Don't mislead: with a missing source, the repo is not "already initialized".
    assert "already initialized" not in captured.out
    assert "no source" in captured.out


def test_init_yaml_error_exits_1_not_traceback(tmp_path, monkeypatch, capsys):
    # A yamlio.YamlError out of the sync engine (missing yq, malformed manifest,
    # or a lefthook-fragment merge failure) must be caught at the CLI boundary →
    # clean exit 1, never a traceback, matching release_sync's contract.
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(init.gh, "repo_root", lambda: str(repo))

    def raise_yaml(root, name):
        raise yamlio.YamlError("yq -o=json . failed (1): bad YAML")

    monkeypatch.setattr(init, "_materialize_config_sources", raise_yaml)
    rc = init.main([])
    err = capsys.readouterr().err
    assert rc == 1
    assert "bad YAML" in err


# --------------------------------------------------------------------------
# --help
# --------------------------------------------------------------------------


def test_init_help_exits_0_and_prints_usage(capsys):
    rc = init.main(["--help"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "Usage:" in out
    assert "release-core init" in out


def test_init_unknown_flag_is_usage_error(capsys):
    rc = init.main(["--nope"])
    assert rc == 64


# --------------------------------------------------------------------------
# self-contained bundle path — compose config from the wheel-bundled templates
# (release_core/_bundled_templates/) with NO release clone. This is the DEFAULT
# path a pip-installed consumer takes; the tests above monkeypatch
# _materialize_config_sources wholesale, so the bundle composition below is the
# coverage for the actual offline machinery (the feat/wheel-self-contained work).
# --------------------------------------------------------------------------

# yq (mikefarah v4) is required for the lefthook fragment merge — the same hard
# dep release-sync has. Skip the merge-dependent tests cleanly if it's absent so
# a yq-less dev box doesn't see spurious failures (CI pins yq, so coverage holds).
import shutil as _shutil  # noqa: E402

import pytest  # noqa: E402

_HAVE_YQ = _shutil.which("yq") is not None
_needs_yq = pytest.mark.skipif(not _HAVE_YQ, reason="yq (mikefarah v4) not installed")

# The repo's real templates/ tree (tests/ -> release_core -> lib -> commons ->
# templates). Used as a faithful bundle so the composition tests exercise the
# ACTUAL fragments the wheel ships, not a hand-rolled stand-in.
_REAL_TEMPLATES = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", "..", "..", ".."))


def _read(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def _fake_bundle(tmp_path) -> str:
    """A minimal but valid bundle tree (templates/...) for the go-cli kind:
    commons lint configs + fragment, the base fragment, a go-quality capability
    fragment, and a go-cli manifest. Returns the tpl_root (…/templates)."""
    root = tmp_path / "_bundled_templates" / "templates"
    (root / "commons").mkdir(parents=True)
    (root / "components" / "go-quality").mkdir(parents=True)
    (root / "go-cli").mkdir(parents=True)

    # Static commons lint configs (copied verbatim by the bundle path).
    for name in (
        ".markdownlint.json",
        ".markdownlintignore",
        ".yamllint",
        ".shellcheckrc",
        ".editorconfig",
        ".prettierignore",
    ):
        (root / "commons" / name).write_text(f"# fake {name}\n")

    (root / "components" / "_lefthook-base.yaml").write_text(
        "pre-commit:\n  parallel: true\n  commands: {}\n"
    )
    (root / "commons" / "lefthook.fragment.yaml").write_text(
        "pre-commit:\n  commands:\n    markdownlint:\n      run: markdownlint .\n"
    )
    (root / "components" / "go-quality" / "lefthook.fragment.yaml").write_text(
        "pre-commit:\n  commands:\n    go-vet:\n      run: go vet ./...\n"
    )
    (root / "go-cli" / "manifest.yaml").write_text("kind: go-cli\ncapabilities:\n  - go-quality\n")
    return str(root)


# ---- _bundle_templates_root --------------------------------------------------


def test_bundle_templates_root_none_when_unstaged(monkeypatch, tmp_path):
    # No staged _bundled_templates/ next to the module → None, so init falls back
    # to the $RELEASE_HOME git path. (Not asserted against the live source tree:
    # a local `python -m build` stages the gitignored bundle on disk, which would
    # make a bare assertion flaky — so we drive the resolver at a clean fake dir.)
    fake_pkg = tmp_path / "release_core" / "verbs"
    fake_pkg.mkdir(parents=True)
    fake_file = fake_pkg / "init.py"
    fake_file.write_text("")
    monkeypatch.setattr(init.os.path, "realpath", lambda _p: str(fake_file))
    assert init._bundle_templates_root() is None


def test_bundle_templates_root_found_when_staged(monkeypatch, tmp_path):
    # Point the resolver at a fake module dir whose _bundled_templates/templates
    # exists; it must return that path.
    fake_pkg = tmp_path / "release_core" / "verbs"
    fake_pkg.mkdir(parents=True)
    bundle = tmp_path / "release_core" / "_bundled_templates" / "templates"
    bundle.mkdir(parents=True)
    fake_file = fake_pkg / "init.py"
    fake_file.write_text("")
    monkeypatch.setattr(init.os.path, "realpath", lambda _p: str(fake_file))
    assert init._bundle_templates_root() == str(bundle)


# ---- _capabilities_from_bundle ----------------------------------------------


@_needs_yq  # _capabilities_from_bundle parses the manifest via yamlio (yq)
def test_capabilities_from_bundle_reads_manifest(tmp_path):
    tpl_root = _fake_bundle(tmp_path)
    assert init._capabilities_from_bundle(tpl_root, "go-cli") == ["go-quality"]


def test_capabilities_from_bundle_missing_manifest_is_empty(tmp_path):
    tpl_root = _fake_bundle(tmp_path)
    assert init._capabilities_from_bundle(tpl_root, "no-such-kind") == []


# ---- _materialize_config_from_bundle (fake bundle) --------------------------


@_needs_yq
def test_materialize_from_bundle_composes_config_and_lefthook(tmp_path):
    tpl_root = _fake_bundle(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    sources = init._materialize_config_from_bundle(tpl_root, str(repo), "go-cli", ["go-quality"])

    # Every static commons config is copied verbatim.
    for name in (
        ".markdownlint.json",
        ".yamllint",
        ".shellcheckrc",
        ".editorconfig",
        ".prettierignore",
        ".markdownlintignore",
    ):
        assert name in sources, f"{name} should be composed from the bundle"
        assert os.path.isfile(sources[name])

    # lefthook.yml is fragment-merged (base < commons < go-quality), carries the
    # "do not edit" provenance header, and contains commands from BOTH fragments.
    assert "lefthook.yml" in sources
    text = _read(sources["lefthook.yml"])
    assert "Generated by release-core init from the bundled templates" in text
    assert "markdownlint" in text  # from commons fragment
    assert "go-vet" in text  # from the go-quality capability fragment


@_needs_yq
def test_materialize_from_bundle_uses_real_templates(tmp_path):
    # The strongest proof: compose from the repo's ACTUAL templates tree exactly
    # as the staged bundle would, for the go-cli kind. Exercises the real
    # fragments the wheel ships, so a fragment-merge regression is caught here.
    repo = tmp_path / "repo"
    repo.mkdir()
    caps = init._capabilities_from_bundle(_REAL_TEMPLATES, "go-cli")
    assert "go-quality" in caps
    sources = init._materialize_config_from_bundle(_REAL_TEMPLATES, str(repo), "go-cli", caps)
    assert "lefthook.yml" in sources
    text = _read(sources["lefthook.yml"])
    # commons gate (markdownlint) + the go-quality capability (gofmt/go-vet).
    assert "markdownlint" in text
    assert "go-vet" in text
    # The full documented config subset is present (commons ships all of them).
    for name in init.CONFIG_FILES:
        assert name in sources, f"{name} missing from real-templates composition"


# ---- _materialize_config_sources routing ------------------------------------


@_needs_yq  # the spy calls the REAL composer, which fragment-merges via yq
def test_materialize_sources_defaults_to_bundle_when_no_release_home(tmp_path, monkeypatch):
    # No $RELEASE_HOME → the bundle path is taken (NOT the git engine).
    tpl_root = _fake_bundle(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.delenv("RELEASE_HOME", raising=False)
    monkeypatch.setattr(init, "_bundle_templates_root", lambda: tpl_root)
    monkeypatch.setattr(init.manifest, "detect_kind", lambda root: "go-cli")

    # Spy: the git engine must NOT be touched on the bundle path.
    def _boom(*a, **k):  # pragma: no cover - asserts non-invocation
        raise AssertionError("git sync engine must not run on the bundle path")

    monkeypatch.setattr(init.sync, "select_ref", _boom)

    called = {}
    real = init._materialize_config_from_bundle

    def _spy(tpl, root, kind, caps):
        called["caps"] = caps
        called["kind"] = kind
        return real(tpl, root, kind, caps)

    monkeypatch.setattr(init, "_materialize_config_from_bundle", _spy)

    sources = init._materialize_config_sources(str(repo), "repo")
    assert called["kind"] == "go-cli"
    assert called["caps"] == ["go-quality"]  # from the bundled manifest
    assert ".yamllint" in sources


@_needs_yq  # capability resolution parses .release-sync.yaml via yamlio (yq)
def test_materialize_sources_bundle_honors_consumer_sync_yaml(tmp_path, monkeypatch):
    # A consumer .release-sync.yaml capability override wins over the bundled
    # manifest default, on the offline path too.
    tpl_root = _fake_bundle(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / ".release-sync.yaml").write_text("capabilities:\n  - go-quality\n  - extra\n")
    monkeypatch.delenv("RELEASE_HOME", raising=False)
    monkeypatch.setattr(init, "_bundle_templates_root", lambda: tpl_root)
    monkeypatch.setattr(init.manifest, "detect_kind", lambda root: "go-cli")

    captured = {}

    def _spy(tpl, root, kind, caps):
        captured["caps"] = caps
        return {}

    monkeypatch.setattr(init, "_materialize_config_from_bundle", _spy)
    init._materialize_config_sources(str(repo), "repo")
    assert captured["caps"] == ["go-quality", "extra"]


def test_materialize_sources_release_home_overrides_bundle(tmp_path, monkeypatch):
    # An explicit $RELEASE_HOME git clone OVERRIDES the bundle: the full sync
    # engine runs (release-dev's live-templates path), bundle untouched.
    tpl_root = _fake_bundle(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    clone = tmp_path / "clone"
    (clone / ".git").mkdir(parents=True)
    monkeypatch.setenv("RELEASE_HOME", str(clone))
    monkeypatch.setattr(init, "_bundle_templates_root", lambda: tpl_root)
    monkeypatch.setattr(init.manifest, "detect_kind", lambda root: "go-cli")

    # The bundle composer must NOT run when a clone is present.
    def _boom(*a, **k):  # pragma: no cover - asserts non-invocation
        raise AssertionError("bundle path must not run when $RELEASE_HOME is a clone")

    monkeypatch.setattr(init, "_materialize_config_from_bundle", _boom)

    # Stub the git engine so the override path is exercised without real git.
    monkeypatch.setattr(init.sync, "select_ref", lambda *a, **k: "abcdef")
    monkeypatch.setattr(init.gh, "git_rev_parse", lambda *a, **k: "abcdef0")
    monkeypatch.setattr(
        init.sync,
        "resolve_capabilities",
        lambda *a, **k: sync.Capabilities(names=["go-quality"], manifest_source="x"),
    )
    monkeypatch.setattr(init.sync, "build_plan", lambda *a, **k: object())

    def _fake_materialize(home, ref, sha, plan, dest):
        for name in init.CONFIG_FILES:
            with open(os.path.join(dest, name), "w", encoding="utf-8") as fh:
                fh.write(f"# git-engine {name}\n")

    monkeypatch.setattr(init.sync, "materialize", _fake_materialize)

    sources = init._materialize_config_sources(str(repo), "repo")
    assert set(sources) == set(init.CONFIG_FILES)
    assert _read(sources["lefthook.yml"]) == "# git-engine lefthook.yml\n"


@_needs_yq
def test_main_end_to_end_through_bundle_path(tmp_path, monkeypatch, capsys):
    # Full main([]) over the offline bundle path: detect kind, compose from the
    # bundle, create-if-absent the documented set — no $RELEASE_HOME, no git.
    tpl_root = _fake_bundle(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.delenv("RELEASE_HOME", raising=False)
    monkeypatch.setattr(init.gh, "repo_root", lambda: str(repo))
    monkeypatch.setattr(init, "_bundle_templates_root", lambda: tpl_root)
    monkeypatch.setattr(init.manifest, "detect_kind", lambda root: "go-cli")

    rc = init.main([])
    out = capsys.readouterr().out
    assert rc == 0
    for name in init.CONFIG_FILES:
        assert (repo / name).is_file(), f"{name} should be materialized offline"
    assert "7 created" in out
    # The composed gate carries the bundle provenance header.
    assert "from the bundled templates" in (repo / "lefthook.yml").read_text()
