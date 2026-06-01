"""detect_kind: every Kind the bash bin/detect-kind classified, plus precedence.

Fixture-driven over real on-disk trees built per test (tmp_path) — exactly the
filesystem signals detect_kind inspects, no mocking. The list of (files, dirs)
→ kind cases is the byte-for-byte contract with the old script.
"""

from __future__ import annotations

import shutil

import pytest
from release_core import manifest

# load_sync_config / kind_manifest read YAML via yamlio → `yq`. detect_kind
# itself touches NO YAML, so its (the bulk) tests never need yq.
yq_required = pytest.mark.skipif(shutil.which("yq") is None, reason="`yq` not on PATH")


def build(root, *, files=(), dirs=(), contents=None):
    """Materialize a fixture tree under `root`."""
    for d in dirs:
        (root / d).mkdir(parents=True, exist_ok=True)
    for f in files:
        p = root / f
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("")
    for path, text in (contents or {}).items():
        p = root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)


def test_brew_tap_formula(tmp_path):
    build(tmp_path, dirs=["Formula"])
    assert manifest.detect_kind(str(tmp_path)) == "brew-tap"


def test_brew_tap_casks(tmp_path):
    build(tmp_path, dirs=["Casks"])
    assert manifest.detect_kind(str(tmp_path)) == "brew-tap"


def test_tree_sitter(tmp_path):
    build(tmp_path, files=["grammar.js"])
    assert manifest.detect_kind(str(tmp_path)) == "tree-sitter"


def test_tauri_app(tmp_path):
    build(tmp_path, files=["src-tauri/Cargo.toml", "package.json"])
    assert manifest.detect_kind(str(tmp_path)) == "tauri-app"


def test_zed_extension(tmp_path):
    build(tmp_path, files=["extension.toml", "Cargo.toml"])
    assert manifest.detect_kind(str(tmp_path)) == "zed-extension"


def test_rust_cli(tmp_path):
    build(tmp_path, files=["Cargo.toml"])
    assert manifest.detect_kind(str(tmp_path)) == "rust-cli"


def test_go_cli(tmp_path):
    build(tmp_path, files=["go.mod"])
    assert manifest.detect_kind(str(tmp_path)) == "go-cli"


def test_electron_app_via_electron_builder(tmp_path):
    build(
        tmp_path,
        contents={"package.json": '{\n  "devDependencies": {"electron-builder": "^1"}\n}\n'},
    )
    assert manifest.detect_kind(str(tmp_path)) == "electron-app"


def test_electron_app_via_electron(tmp_path):
    build(tmp_path, contents={"package.json": '{"dependencies": {"electron": "^30"}}'})
    assert manifest.detect_kind(str(tmp_path)) == "electron-app"


def test_vscode_ext_via_vsce_scoped(tmp_path):
    build(tmp_path, contents={"package.json": '{"devDependencies": {"@vscode/vsce": "^2"}}'})
    assert manifest.detect_kind(str(tmp_path)) == "vscode-ext"


def test_vscode_ext_via_vsce(tmp_path):
    build(tmp_path, contents={"package.json": '{"devDependencies": {"vsce": "^1"}}'})
    assert manifest.detect_kind(str(tmp_path)) == "vscode-ext"


def test_github_action_yml(tmp_path):
    build(tmp_path, files=["action.yml"])
    assert manifest.detect_kind(str(tmp_path)) == "github-action"


def test_github_action_yaml(tmp_path):
    build(tmp_path, files=["action.yaml"])
    assert manifest.detect_kind(str(tmp_path)) == "github-action"


def test_nvim_plugin_classic_with_lua_dir(tmp_path):
    build(tmp_path, dirs=["plugin", "lua"])
    assert manifest.detect_kind(str(tmp_path)) == "nvim-plugin"


def test_nvim_plugin_modern_lua_file(tmp_path):
    # No lua/ dir, but a *.lua within depth 3 (queries/ triggers the branch).
    build(tmp_path, dirs=["queries"], files=["lua/mod/init.lua"])
    assert manifest.detect_kind(str(tmp_path)) == "nvim-plugin"


def test_nvim_plugin_via_ftdetect(tmp_path):
    build(tmp_path, dirs=["ftdetect", "lua"])
    assert manifest.detect_kind(str(tmp_path)) == "nvim-plugin"


def test_docs_site(tmp_path):
    build(tmp_path, files=["mkdocs.yml"])
    assert manifest.detect_kind(str(tmp_path)) == "docs-site"


def test_static_site_book(tmp_path):
    build(tmp_path, files=["book.toml"])
    assert manifest.detect_kind(str(tmp_path)) == "static-site"


def test_static_site_jekyll(tmp_path):
    build(tmp_path, files=["_config.yml"])
    assert manifest.detect_kind(str(tmp_path)) == "static-site"


# ---- precedence (the WHY-ordered guards in the bash) -------------------


def test_tauri_precedes_rust_cli_and_electron(tmp_path):
    # src-tauri/Cargo.toml + package.json with electron in deps → still tauri.
    build(
        tmp_path,
        files=["src-tauri/Cargo.toml", "Cargo.toml"],
        contents={"package.json": '{"devDependencies": {"electron": "^30"}}'},
    )
    assert manifest.detect_kind(str(tmp_path)) == "tauri-app"


def test_zed_precedes_rust_cli(tmp_path):
    build(tmp_path, files=["extension.toml", "Cargo.toml"])
    assert manifest.detect_kind(str(tmp_path)) == "zed-extension"


def test_tree_sitter_precedes_nvim(tmp_path):
    # grammar.js wins even with a plugin/ + lua/ layout present.
    build(tmp_path, files=["grammar.js", "lua/x.lua"], dirs=["plugin"])
    assert manifest.detect_kind(str(tmp_path)) == "tree-sitter"


def test_mkdocs_precedes_static_site(tmp_path):
    build(tmp_path, files=["mkdocs.yml", "_config.yml"])
    assert manifest.detect_kind(str(tmp_path)) == "docs-site"


def test_nvim_plugin_dir_without_lua_is_not_nvim(tmp_path):
    # A plugin/ dir but NO lua source → falls through (not nvim-plugin).
    build(tmp_path, dirs=["plugin"], files=["mkdocs.yml"])
    assert manifest.detect_kind(str(tmp_path)) == "docs-site"


def test_lua_beyond_depth_3_does_not_count(tmp_path):
    # find -maxdepth 3: a *.lua at find-depth 4 must NOT trigger nvim-plugin.
    build(tmp_path, dirs=["plugin"], files=["a/b/c/deep.lua"])
    with pytest.raises(manifest.KindError):
        manifest.detect_kind(str(tmp_path))


def test_lua_at_depth_3_counts(tmp_path):
    # a/b/c.lua is at find-depth 3 → counts.
    build(tmp_path, dirs=["queries"], files=["a/b/c.lua"])
    assert manifest.detect_kind(str(tmp_path)) == "nvim-plugin"


def test_undetermined_raises(tmp_path):
    build(tmp_path, files=["README.md"])
    with pytest.raises(manifest.KindError) as exc:
        manifest.detect_kind(str(tmp_path))
    assert "could not detect kind" in str(exc.value)


def test_package_json_without_signal_falls_through(tmp_path):
    # A plain package.json (no electron/vsce marker) is not a recognised Kind.
    build(tmp_path, contents={"package.json": '{"name": "plain"}'})
    with pytest.raises(manifest.KindError):
        manifest.detect_kind(str(tmp_path))


# ---- load_sync_config / kind_manifest ----------------------------------


def test_load_sync_config_absent(tmp_path):
    assert manifest.load_sync_config(str(tmp_path)) == {}


@yq_required
def test_load_sync_config_parses(tmp_path):
    (tmp_path / ".release-sync.yaml").write_text("capabilities:\n  - mkdocs\n  - bats\n")
    cfg = manifest.load_sync_config(str(tmp_path))
    assert cfg == {"capabilities": ["mkdocs", "bats"]}


def test_kind_manifest_absent(tmp_path):
    assert manifest.kind_manifest("rust-cli", str(tmp_path)) == {}


@yq_required
def test_kind_manifest_parses(tmp_path):
    mdir = tmp_path / "templates" / "rust-cli"
    mdir.mkdir(parents=True)
    (mdir / "manifest.yaml").write_text("capabilities:\n  - cargo\n")
    assert manifest.kind_manifest("rust-cli", str(tmp_path)) == {"capabilities": ["cargo"]}
