"""release_advance_major verb — major auto-detection, ff-merge decision, and
the push/dry-run/refusal branches.

Offline: the git boundary is stubbed at proc.run/proc.out with a recorded
command→result map (a tiny fake git "server" state), never at subprocess. The
fast-forward decision (ancestor check), the highest-vN auto-detect, and the
arg loop are the testable core.
"""

from __future__ import annotations

import subprocess

import pytest
from release_core import proc
from release_core.verbs import release_advance_major as ram


def _completed(cmd, *, returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(cmd, returncode, stdout, stderr)


class FakeGit:
    """A minimal git fake: maps the porcelain release-advance-major issues to
    recorded results. Records every push so tests can assert on it."""

    def __init__(
        self,
        *,
        origin="git@github.com:arthur-debert/release.git",
        branches=("origin/v1", "origin/v2"),
        revs=None,
        ancestor=True,
        major_exists=True,
    ):
        self.origin = origin
        self.branches = list(branches)
        # ref -> full sha
        self.revs = revs or {
            "origin/main": "aaaaaaaaaaaa",
            "origin/v2": "bbbbbbbbbbbb",
        }
        self.ancestor = ancestor
        self.major_exists = major_exists
        self.pushes: list[list[str]] = []

    def run(self, cmd, **kw):  # noqa: C901 — flat git command switch
        if cmd[:1] != ["git"]:
            raise AssertionError(f"unexpected non-git proc.run: {cmd}")
        rest = cmd[1:]
        if rest[:2] == ["remote", "get-url"]:
            return _completed(cmd, returncode=0, stdout=self.origin + "\n")
        if rest[0] == "fetch":
            return _completed(cmd, returncode=0)
        if rest[:2] == ["branch", "-r"]:
            return _completed(cmd, returncode=0, stdout="\n".join(self.branches) + "\n")
        if rest[:3] == ["rev-parse", "--verify", "--quiet"]:
            ref = rest[3]
            short = ref.split("/")[-1]
            if short.startswith("v") and not self.major_exists:
                return _completed(cmd, returncode=1, stdout="")
            sha = self.revs.get(ref)
            return _completed(cmd, returncode=0 if sha else 1, stdout=(sha or "") + "\n")
        if rest[:2] == ["rev-parse", "--verify"]:
            sha = self.revs.get(rest[2])
            return _completed(cmd, returncode=0 if sha else 1, stdout=(sha or "") + "\n")
        if rest[:2] == ["rev-parse", "--short"]:
            return _completed(cmd, returncode=0, stdout=rest[2][:7] + "\n")
        if rest[0] == "rev-parse":
            sha = self.revs.get(rest[1], rest[1])
            return _completed(cmd, returncode=0, stdout=sha + "\n")
        if rest[:2] == ["merge-base", "--is-ancestor"]:
            return _completed(cmd, returncode=0 if self.ancestor else 1)
        if rest[0] == "push":
            self.pushes.append(cmd)
            return _completed(cmd, returncode=0)
        raise AssertionError(f"unhandled git cmd: {cmd}")

    def out(self, cmd, **kw):
        return self.run(cmd, **kw).stdout.strip()


@pytest.fixture
def in_release_repo(tmp_path, monkeypatch):
    """Pretend we are inside a release clone (RELEASE_HOME with a .git)."""
    (tmp_path / ".git").mkdir()
    monkeypatch.setenv("RELEASE_HOME", str(tmp_path))
    return tmp_path


def _wire(monkeypatch, fake):
    monkeypatch.setattr(proc, "run", fake.run)
    monkeypatch.setattr(proc, "out", fake.out)


# --- help / usage -----------------------------------------------------


def test_help_exits_0(capsys):
    assert ram.main(["--help"]) == 0
    assert "floating major branch" in capsys.readouterr().out


def test_unknown_arg_is_usage_error(capsys):
    assert ram.main(["--bogus"]) == 64
    assert "unknown arg" in capsys.readouterr().err


def test_too_many_positionals_is_usage_error(in_release_repo, monkeypatch, capsys):
    _wire(monkeypatch, FakeGit())
    assert ram.main(["origin/main", "extra"]) == 64
    assert "too many args" in capsys.readouterr().err


def test_bad_major_flag_is_usage_error(in_release_repo, monkeypatch, capsys):
    _wire(monkeypatch, FakeGit())
    assert ram.main(["--major", "release"]) == 64
    assert "must look like vN" in capsys.readouterr().err


# --- refusal: wrong origin --------------------------------------------


def test_refuses_non_release_origin(in_release_repo, monkeypatch, capsys):
    _wire(monkeypatch, FakeGit(origin="git@github.com:someone/else.git"))
    assert ram.main([]) == 1
    assert "refusing" in capsys.readouterr().err


# --- auto-detect highest major ----------------------------------------


def test_advances_highest_major_by_default(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(
        branches=["origin/v1", "origin/v2", "origin/main"],
        revs={"origin/main": "aaaaaaaaaaaa", "origin/v2": "bbbbbbbbbbbb"},
        ancestor=True,
    )
    _wire(monkeypatch, fake)
    assert ram.main([]) == 0
    out = capsys.readouterr().out
    assert "advancing v2:" in out
    assert fake.pushes == [["git", "push", "origin", "aaaaaaaaaaaa:refs/heads/v2"]]


def test_no_vN_branch_errors(in_release_repo, monkeypatch, capsys):
    _wire(monkeypatch, FakeGit(branches=["origin/main"]))
    assert ram.main([]) == 1
    assert "no origin/vN branch found" in capsys.readouterr().err


# --- already up to date -----------------------------------------------


def test_already_up_to_date_is_noop(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(revs={"origin/main": "cccccccccccc", "origin/v2": "cccccccccccc"})
    _wire(monkeypatch, fake)
    assert ram.main([]) == 0
    assert "already at" in capsys.readouterr().out
    assert fake.pushes == []


# --- non-fast-forward refusal -----------------------------------------


def test_non_ff_refuses(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(ancestor=False)
    _wire(monkeypatch, fake)
    assert ram.main([]) == 1
    err = capsys.readouterr().err
    assert "NOT an ancestor" in err
    assert "fast-forward is impossible" in err
    assert fake.pushes == []


# --- dry-run pushes nothing -------------------------------------------


def test_dry_run_pushes_nothing(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(ancestor=True)
    _wire(monkeypatch, fake)
    assert ram.main(["--dry-run"]) == 0
    out = capsys.readouterr().out
    assert "(dry-run) would: git push origin" in out
    assert fake.pushes == []


# --- explicit --major + explicit ref ----------------------------------


def test_explicit_major_and_ref(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(
        revs={"deadbeefcafe": "deadbeefcafe", "origin/v1": "111111111111"},
        ancestor=True,
    )
    _wire(monkeypatch, fake)
    assert ram.main(["--major", "v1", "deadbeefcafe"]) == 0
    assert fake.pushes == [["git", "push", "origin", "deadbeefcafe:refs/heads/v1"]]


# --- major branch doesn't exist yet -----------------------------------


def test_creates_missing_major(in_release_repo, monkeypatch, capsys):
    fake = FakeGit(major_exists=False, ancestor=True)
    _wire(monkeypatch, fake)
    assert ram.main(["--major", "v3"]) == 0
    err = capsys.readouterr().err
    assert "doesn't exist yet; creating it" in err
    assert fake.pushes == [["git", "push", "origin", "aaaaaaaaaaaa:refs/heads/v3"]]


# --- not in release repo, no RELEASE_HOME -----------------------------


def test_not_in_release_repo_errors(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("RELEASE_HOME", str(tmp_path / "nope"))

    def fake_run(cmd, **kw):
        if cmd[:3] == ["git", "rev-parse", "--show-toplevel"]:
            return _completed(cmd, returncode=128, stdout="", stderr="not a repo")
        raise AssertionError(f"unexpected: {cmd}")

    monkeypatch.setattr(proc, "run", fake_run)
    assert ram.main([]) == 1
    assert "not inside the release repo" in capsys.readouterr().err


# --- highest-major helper ---------------------------------------------


def test_highest_major_sorts_numerically(monkeypatch):
    def fake_run(cmd, **kw):
        return _completed(
            cmd,
            returncode=0,
            stdout="  origin/v2\n  origin/v10\n  origin/v1\n  origin/main\n",
        )

    monkeypatch.setattr(proc, "run", fake_run)
    # v10 > v2 numerically (sort -V), not lexically.
    assert ram._highest_major() == "v10"
