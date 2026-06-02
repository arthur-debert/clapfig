"""list_repo_scripts verb — file counting, column rendering, dir selection.

Offline filesystem only. Exercises the count-real-files (broken-symlink counts),
col-lines placeholders, and the column/header machinery against a synthetic tree.
"""

from __future__ import annotations

import os

from release_core.verbs import list_repo_scripts


def test_count_real_files_counts_broken_symlinks(tmp_path):
    d = tmp_path / "bin"
    d.mkdir()
    (d / "real").write_text("x")
    os.symlink(tmp_path / "does-not-exist", d / "broken")
    # broken symlink fails os.path.exists but is an islink → still counts
    assert list_repo_scripts._count_real_files(str(d)) == 2


def test_count_real_files_absent_dir_is_zero(tmp_path):
    assert list_repo_scripts._count_real_files(str(tmp_path / "nope")) == 0


def test_col_lines_not_present(tmp_path):
    assert list_repo_scripts._col_lines(str(tmp_path / "nope")) == ["(not present)"]


def test_col_lines_empty(tmp_path):
    d = tmp_path / "scripts"
    d.mkdir()
    assert list_repo_scripts._col_lines(str(d)) == ["(empty)"]


def test_col_lines_sorted_contents(tmp_path):
    d = tmp_path / "bin"
    d.mkdir()
    (d / "b").write_text("")
    (d / "a").write_text("")
    assert list_repo_scripts._col_lines(str(d)) == ["a", "b"]


def test_print_columns_header_and_rows(capsys):
    list_repo_scripts._print_columns(
        [["a", "b"], ["x"]],
        ["bin", "scripts"],
    )
    out = capsys.readouterr().out.splitlines()
    assert out[0].startswith("bin/")
    assert "scripts/" in out[0]
    # row 0: a + x; row 1: b + (blank for scripts)
    assert out[2].startswith("a")
    assert "x" in out[2]
    assert out[3].startswith("b")


def test_main_owner_filter_and_totals_footer(tmp_path, monkeypatch, capsys):
    # Build a fake fleet: one lex-fmt repo with a bin/ dir.
    repo = tmp_path / "lex-fmt" / "lex"
    (repo / "bin").mkdir(parents=True)
    (repo / "bin" / "tool").write_text("")

    monkeypatch.setattr(
        list_repo_scripts,
        "_repos",
        lambda: [("lex-fmt/lex", str(repo)), ("arthur-debert/x", str(tmp_path / "absent"))],
    )
    rc = list_repo_scripts.main(["--owner", "lex-fmt"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "=== lex-fmt/lex ===" in out
    assert "=== arthur-debert/x ===" not in out  # filtered out
    assert "--- totals across fleet ---" in out
    assert "bin/     1" in out


def test_main_only_present_skips_absent(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(
        list_repo_scripts,
        "_repos",
        lambda: [("o/absent", str(tmp_path / "nope"))],
    )
    rc = list_repo_scripts.main(["--only-present"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "o/absent" not in out


def test_main_absent_repo_without_only_present_shows_placeholder(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(
        list_repo_scripts,
        "_repos",
        lambda: [("o/absent", str(tmp_path / "nope"))],
    )
    assert list_repo_scripts.main([]) == 0
    out = capsys.readouterr().out
    assert "(repo not cloned locally)" in out


def test_main_only_bin_shows_only_bin_footer(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(list_repo_scripts, "_repos", lambda: [])
    list_repo_scripts.main(["--only-bin"])
    out = capsys.readouterr().out
    assert "bin/" in out
    assert "scripts/" not in out
    assert "app-bin/" not in out


def test_main_unknown_arg_exits_64(capsys):
    rc = list_repo_scripts.main(["--nope"])
    assert rc == 64


def test_main_help_exits_0(capsys):
    rc = list_repo_scripts.main(["--help"])
    assert rc == 0
    assert "Usage:" in capsys.readouterr().out
