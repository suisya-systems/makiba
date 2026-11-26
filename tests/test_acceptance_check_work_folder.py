"""Issue #558: the refusals of scripts/acceptance-check.sh about its work folder, which it makes
under the repository's git folder (<git common dir>/acceptance-check/). It fails at once, before
any test runs, when
  - the git folder is under a temporary folder while the checkout is not (a separate git folder,
    or a linked worktree of a repository in a temporary folder): declared tests would see a path
    the tests job does not;
  - a folder above the work folder, other than CARGO_HOME, holds .cargo/config or
    .cargo/config.toml: Cargo would read it in the builds of both the test commit and head.
It proceeds when the repository itself is in a temporary folder, and when the config above it is
CARGO_HOME's. The declared tests are Python here, so no case builds a Cargo package. The test of
where the declared tests are built is tests/test_acceptance_check_outside_tmp.py."""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "acceptance-check.sh"
TEST_BODY = "from lib import answer\n\n\ndef test_answer():\n    assert answer() == 2\n"


def env(**extra: str) -> dict[str, str]:
    drop = ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR")
    return {
        **{k: v for k, v in os.environ.items() if k not in drop and not k.startswith("GIT_")},
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@t",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@t",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        **extra,
    }


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-c", "commit.gpgsign=false", *args],
        cwd=cwd,
        env=env(),
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def write(root: Path, rel: str, text: str) -> None:
    f = root / rel
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text(text, encoding="utf-8")


def common_dir(tree: Path) -> Path:
    return Path(git(tree, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()).resolve()


def branch(repo: Path) -> tuple[str, str]:
    """In an initialised, empty repository: a base on main, then on feat/x a test commit declaring
    tests/test_answer.py and a fix. Returns (base, head)."""
    write(repo, "lib.py", "def answer():\n    return 1\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    base = git(repo, "rev-parse", "HEAD").strip()
    git(repo, "checkout", "-q", "-b", "feat/x")
    write(repo, "tests/test_answer.py", TEST_BODY)
    git(repo, "add", "tests/test_answer.py")
    git(repo, "commit", "-q", "-m", "test: declare\n\nAcceptance-Test: tests/test_answer.py\nCo-Authored-By: t <t@t>")
    write(repo, "lib.py", "def answer():\n    return 2\n")
    git(repo, "commit", "-q", "-am", "fix")
    return base, git(repo, "rev-parse", "HEAD").strip()


def check(repo: Path, base: str, head: str, **extra: str) -> tuple[int, str]:
    r = subprocess.run(
        ["bash", str(SCRIPT), base, head],
        cwd=repo,
        env=env(**extra),
        check=False,
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout + r.stderr


def last(out: str) -> str:
    return out.strip().splitlines()[-1] if out.strip() else ""


def left_nothing(gitdir: Path, repo: Path, out: str) -> None:
    parent = gitdir / "acceptance-check"
    assert not parent.exists() or not any(parent.iterdir()), (sorted(map(str, parent.iterdir())), out)
    worktrees = [w for w in git(repo, "worktree", "list", "--porcelain").splitlines() if w.startswith("worktree ")]
    assert len(worktrees) == (2 if repo.name == "linked" else 1), (worktrees, out)


@pytest.fixture
def outside(tmp_path):
    """A fresh folder outside every temporary folder: under the git folder of the tree holding this
    file (the checkout's .git in CI and in a makiba worktree)."""
    d = Path(tempfile.mkdtemp(prefix="acceptance-work-folder.", dir=common_dir(ROOT))).resolve()
    yield d
    shutil.rmtree(d, ignore_errors=True)


# The git folder in a temporary folder while the checkout is not


def test_separate_git_folder_in_a_temporary_folder_is_refused(tmp_path, outside):
    tmpdir = tmp_path / "tmpdir"
    tmpdir.mkdir()
    gitdir = (tmpdir / "sep.git").resolve()
    repo = outside / "r"
    subprocess.run(
        ["git", "init", "-q", "-b", "main", "--separate-git-dir", str(gitdir), str(repo)],
        env=env(),
        check=True,
        capture_output=True,
    )
    base, head = branch(repo)
    rc, out = check(repo, base, head, TMPDIR=str(tmpdir))
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert f"the git folder {gitdir} is under a temporary folder ({tmpdir.resolve()}) while the checkout {repo.resolve()} is not" in out, out
    assert "passes at head" not in out and "fails at the test commit" not in out, out
    left_nothing(gitdir, repo, out)


def test_linked_worktree_of_a_repository_in_a_temporary_folder_is_refused(tmp_path, outside):
    tmpdir = tmp_path / "tmpdir"
    tmpdir.mkdir()
    main = tmpdir / "main"
    main.mkdir()
    git(main, "init", "-q", "-b", "main")
    base, head = branch(main)
    git(main, "checkout", "-q", "main")
    linked = outside / "linked"
    git(main, "worktree", "add", "-q", "--detach", str(linked), head)
    rc, out = check(linked, base, head, TMPDIR=str(tmpdir))
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert f"while the checkout {linked.resolve()} is not" in out, out
    left_nothing(common_dir(main), linked, out)


def test_repository_in_a_temporary_folder_proceeds(tmp_path):
    tmpdir = tmp_path / "tmpdir"
    repo = tmpdir / "r"
    repo.mkdir(parents=True)
    git(repo, "init", "-q", "-b", "main")
    base, head = branch(repo)
    rc, out = check(repo, base, head, TMPDIR=str(tmpdir))
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert "tests/test_answer.py passes at head" in out, out
    left_nothing(common_dir(repo), repo, out)


# A Cargo config above the work folder


@pytest.mark.parametrize("name", ["config.toml", "config"])
def test_cargo_config_in_the_checkout_is_refused(tmp_path, name):
    repo = tmp_path / "r"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    write(repo, f".cargo/{name}", '[env]\nLEAK = "from-outer"\n')
    base, head = branch(repo)
    rc, out = check(repo, base, head, CARGO_HOME=str(tmp_path / "cargo-home"))
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert f"acceptance-check: {repo.resolve()}/.cargo/{name} lies above the work folder" in out, out
    assert "passes at head" not in out and "fails at the test commit" not in out, out
    left_nothing(common_dir(repo), repo, out)


def test_cargo_config_of_cargo_home_above_the_work_folder_proceeds(tmp_path):
    home = tmp_path / "home"
    write(home, ".cargo/config.toml", '[env]\nLEAK = "from-home"\n')
    repo = home / "r"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    base, head = branch(repo)
    rc, out = check(repo, base, head, CARGO_HOME=str(home / ".cargo"))
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert "lies above the work folder" not in out, out
    left_nothing(common_dir(repo), repo, out)
