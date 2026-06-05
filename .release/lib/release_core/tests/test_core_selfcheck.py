"""selfcheck verb — the runtime canary that release_core's third-party deps
(click) resolved at install.

Asserts the command genuinely uses click (imports the real module), reports the
resolved click version, exits 0, honors -h/--help, and rejects stray args. Also
checks it routes through the top-level `release-core` dispatcher.
"""

from __future__ import annotations

import click
from release_core import cli_entry
from release_core.verbs import selfcheck


def test_binds_the_real_click_module():
    # If --no-deps ever sneaks back into the boot, `import click` at module load
    # fails and this whole module won't import — making the regression loud.
    assert selfcheck.click is click


def test_reports_click_version_and_exits_0(capsys):
    rc = selfcheck.main([])
    out = capsys.readouterr().out
    assert rc == 0
    assert "dependencies OK" in out
    assert "click" in out


def test_help_prints_usage_exit_0(capsys):
    for flag in ("-h", "--help"):
        rc = selfcheck.main([flag])
        out = capsys.readouterr().out
        assert rc == 0
        assert "selfcheck" in out


def test_rejects_unexpected_positional_exit_64():
    assert selfcheck.main(["bogus"]) == 64


def test_routes_through_cli_entry(capsys):
    rc = cli_entry.main(["selfcheck"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "dependencies OK" in out
