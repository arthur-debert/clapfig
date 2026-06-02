"""Hatch build hook: bundle the config templates into the wheel.

`release-core init` materializes the per-repo committed config (lefthook.yml +
lint configs) from these templates. For the pull-model the wheel must be
self-contained — it ships the template DATA so `init` needs no release clone.

The templates live OUTSIDE the package build root (at <repo>/templates/...),
so we stage the config-relevant subset into release_core/_bundled_templates/
at build time; hatch then packages it like any in-package data. The staged
dir is gitignored (a build artifact, never committed).

Only the config-composition inputs are bundled (keeps the wheel small):
  templates/commons/<lint configs> + lefthook.fragment.yaml
  templates/components/<cap>/lefthook.fragment.yaml + _lefthook-base.yaml
  templates/<kind>/{lefthook.fragment.yaml,manifest.yaml}
"""

from __future__ import annotations

import os
import shutil

from hatchling.builders.hooks.plugin.interface import BuildHookInterface

# Files under templates/commons/ that init copies verbatim (kind-independent).
_COMMONS_CONFIGS = (
    ".markdownlint.json",
    ".markdownlintignore",
    ".yamllint",
    ".shellcheckrc",
    ".editorconfig",
    ".prettierignore",
    "lefthook.fragment.yaml",
)

_BUNDLE_DIRNAME = "_bundled_templates"


class CustomBuildHook(BuildHookInterface):
    PLUGIN_NAME = "custom"

    def initialize(self, version, build_data):
        # self.root = the project dir (…/templates/commons/lib/release_core).
        repo_templates = os.path.normpath(os.path.join(self.root, "..", "..", ".."))  # …/templates
        if os.path.basename(repo_templates) != "templates":
            raise RuntimeError(f"hatch_build: expected …/templates, got {repo_templates!r}")

        dest_root = os.path.join(self.root, "release_core", _BUNDLE_DIRNAME, "templates")
        shutil.rmtree(os.path.join(self.root, "release_core", _BUNDLE_DIRNAME), ignore_errors=True)

        def _copy(rel_src: str) -> None:
            src = os.path.join(repo_templates, rel_src)
            if not os.path.isfile(src):
                return
            dst = os.path.join(dest_root, rel_src)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copyfile(src, dst)

        # commons lint configs + fragment
        for name in _COMMONS_CONFIGS:
            _copy(os.path.join("commons", name))

        # capability fragments + the base fragment
        _copy(os.path.join("components", "_lefthook-base.yaml"))
        components = os.path.join(repo_templates, "components")
        if os.path.isdir(components):
            for cap in sorted(os.listdir(components)):
                _copy(os.path.join("components", cap, "lefthook.fragment.yaml"))

        # per-kind gate fragment + manifest (kind dirs are the non-commons,
        # non-components top-level dirs that carry a lefthook fragment).
        for entry in sorted(os.listdir(repo_templates)):
            if entry in ("commons", "components"):
                continue
            kdir = os.path.join(repo_templates, entry)
            if not os.path.isdir(kdir):
                continue
            _copy(os.path.join(entry, "lefthook.fragment.yaml"))
            _copy(os.path.join(entry, "manifest.yaml"))

        # Ensure hatch includes the staged tree in the wheel.
        build_data.setdefault("artifacts", []).append(f"release_core/{_BUNDLE_DIRNAME}/**")

    def finalize(self, version, build_data, artifact_path):
        # The staged tree only needs to exist WHILE hatch packages the wheel.
        # Remove it afterwards so a build leaves no trace in the source tree —
        # otherwise a stale bundle next to an editable install would make
        # `release-core init` silently take the offline path. It is gitignored,
        # so this is hygiene, not correctness for git; ignore_errors keeps a
        # cleanup hiccup from failing an otherwise-good build.
        shutil.rmtree(os.path.join(self.root, "release_core", _BUNDLE_DIRNAME), ignore_errors=True)
