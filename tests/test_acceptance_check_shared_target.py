"""Issue #232: the acceptance checker gives each of its worktrees a target folder of its own.
The checker is called here with CARGO_TARGET_DIR (or CARGO_BUILD_TARGET_DIR) in its own
environment, unlike tests/test_acceptance_check.py, whose ENV drops both. The scratch
repositories come from that file's helpers."""

import subprocess
from pathlib import Path

import pytest

from test_acceptance_check import ENV, LIB_RS, SCRIPT, Repo, last


def check(repo: Repo, **env: str) -> tuple[int, str]:
    r = subprocess.run(
        ["bash", str(SCRIPT), repo.base, repo.rev("HEAD")],
        cwd=repo.path,
        env={**ENV, **env},
        check=False,
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout + r.stderr


def folders(out: str, where: str) -> list[str]:
    """The target folders the checker says it built in, at `where` (test commit | head)."""
    mark = f" at {where} in target folder "
    return [line.split(mark, 1)[1] for line in out.splitlines() if mark in line]


def snapshot(d: Path) -> dict[str, tuple[int, int]]:
    return {
        str(p.relative_to(d)): (p.stat().st_size, p.stat().st_mtime_ns)
        for p in sorted(d.rglob("*"))
    }


@pytest.mark.parametrize("var", ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"])
def test_passing_rust_change_is_pass_with_an_exported_target_folder(tmp_path, var):
    repo = Repo(tmp_path / "r")
    shared = tmp_path / "shared"
    shared.mkdir()
    repo.declare_rs("tests/answer.rs")
    repo.fix()
    rc, out = check(repo, **{var: str(shared)})
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert "tests/answer.rs passes at head" in out, out
    t, h = folders(out, "the test commit"), folders(out, "head")
    assert len(t) == 1 and len(h) == 1, out
    assert t[0].endswith("/t/target") and h[0].endswith("/h/target") and t[0] != h[0], out
    assert not t[0].startswith(str(shared)) and not h[0].startswith(str(shared)), out


def test_runs_sharing_an_exported_folder_do_not_influence_each_other(tmp_path):
    shared = tmp_path / "shared"
    # Another run's passing build of the same crate already sits in the shared folder.
    other = Repo(tmp_path / "other")
    other.declare_rs("tests/answer.rs")
    other.fix()
    subprocess.run(
        ["cargo", "test", "--locked", "--no-run", "--test", "answer"],
        cwd=other.path,
        env={**ENV, "CARGO_TARGET_DIR": str(shared)},
        check=True,
        capture_output=True,
    )
    before = snapshot(shared)
    assert before
    # This run's change still fails at head.
    repo = Repo(tmp_path / "r")
    repo.declare_rs("tests/answer.rs")
    repo.write("src/lib.rs", LIB_RS % 3)
    repo.g("commit", "-q", "-am", "wrong fix")
    rc, out = check(repo, CARGO_TARGET_DIR=str(shared))
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert "tests/answer.rs fails at head" in out, out
    assert snapshot(shared) == before, out
    t, h = folders(out, "the test commit"), folders(out, "head")
    assert len(t) == 1 and len(h) == 1, out
    assert t[0].endswith("/t/target") and h[0].endswith("/h/target") and t[0] != h[0], out
    assert not t[0].startswith(str(shared)) and not h[0].startswith(str(shared)), out
