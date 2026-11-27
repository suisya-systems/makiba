"""Acceptance test for Issue 576: rule 4 of scripts/acceptance-check.sh protects test
infrastructure outside tests/.

Rule 4 (paths protected after the test commit) protected test infrastructure only under tests/.
The merge gate (src/daemon/gate.rs is_infra) protects it by file name wherever it is: a name of
INFRA_NAMES (pytest.ini, conftest.py, pyproject.toml, ...) or one starting with INFRA_PREFIXES
(vitest.config., ...). The checker judges by the same names, at any depth, so a root pytest.ini
added after the test commit fails the check, and a fix that leaves such files alone passes. A base
merge still counts only for its own resolution, and the test commit's rule is unchanged.

Each case builds a scratch repository (a base on main, a test commit declaring tests/test_new.py
on feat/x, a fix) and runs the checker on it, as tests/test_acceptance_helper_change.py does."""

import os
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "acceptance-check.sh"

ENV = {
    **{k: v for k, v in os.environ.items() if k not in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")},
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@t",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@t",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}
TEST_BODY = "from lib import answer\n\n\ndef test_answer():\n    assert answer() == 2\n"
FIX = {"lib.py": "def answer():\n    return 2\n"}
PYTEST_INI = "[pytest]\n"


class Repo:
    """A scratch repository: base on main (lib.py answers 1, a root pyproject.toml), then a
    checked-out branch feat/x."""

    def __init__(self, path: Path):
        self.path = path
        path.mkdir()
        self.g("init", "-q", "-b", "main")
        self.write("lib.py", "def answer():\n    return 1\n")
        self.write("pyproject.toml", "[project]\n")
        self.g("add", "-A")
        self.g("commit", "-q", "-m", "base")
        self.base = self.rev("HEAD")
        self.g("checkout", "-q", "-b", "feat/x")

    def g(self, *args: str) -> str:
        r = subprocess.run(
            ["git", "-c", "commit.gpgsign=false", *args],
            cwd=self.path,
            env=ENV,
            check=False,
            capture_output=True,
            text=True,
        )
        assert r.returncode == 0, f"git {args}: {r.stdout}{r.stderr}"
        return r.stdout

    def rev(self, ref: str) -> str:
        return self.g("rev-parse", ref).strip()

    def write(self, rel: str, text: str) -> None:
        f = self.path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")

    def commit(self, msg: str, files: dict[str, str | None]) -> None:
        """Write (or, for None, remove) each file and commit them."""
        for rel, text in files.items():
            if text is None:
                self.g("rm", "-q", "--", rel)
            else:
                self.write(rel, text)
                self.g("add", "--", rel)
        self.g("commit", "-q", "-m", msg)

    def declare(self, files: dict[str, str] | None = None) -> None:
        """The test commit: tests/test_new.py and any other files, declaring tests/test_new.py."""
        files = {"tests/test_new.py": TEST_BODY, **(files or {})}
        for rel, text in files.items():
            self.write(rel, text)
            self.g("add", "--", rel)
        self.g(
            "commit",
            "-q",
            "-m",
            "test: declare\n\nAcceptance-Test: tests/test_new.py\nCo-Authored-By: t <t@t>",
        )

    def fix(self, files: dict[str, str | None] | None = None) -> None:
        self.commit("fix", {**FIX, **(files or {})})

    def check(self) -> tuple[int, str]:
        r = subprocess.run(
            ["bash", str(SCRIPT), self.base, self.rev("HEAD")],
            cwd=self.path,
            env=ENV,
            check=False,
            capture_output=True,
            text=True,
        )
        return r.returncode, r.stdout + r.stderr


def last(out: str) -> str:
    return out.strip().splitlines()[-1] if out.strip() else ""


def protected_line(path: str) -> str:
    return f"acceptance-check: {path} changed after the test commit ("


@pytest.fixture
def repo(tmp_path):
    return Repo(tmp_path / "r")


def test_root_pytest_ini_added_after_test_commit_fails(repo):
    repo.declare()
    repo.fix({"pytest.ini": PYTEST_INI})
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert protected_line("pytest.ini") in out, out


def test_fix_leaving_infrastructure_alone_passes(repo):
    repo.declare()
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert "changed after the test commit (" not in out, out


@pytest.mark.parametrize(
    "path, text",
    [
        ("conftest.py", "# root conftest\n"),
        ("pyproject.toml", "[project]\nname = 'x'\n"),
        ("pyproject.toml", None),
        ("setup.cfg", "[tool:pytest]\n"),
        ("tox.ini", "[pytest]\n"),
        ("package.json", "{}\n"),
        ("vitest.config.ts", "export default {}\n"),
        ("src/conftest.py", "# nested conftest\n"),
    ],
    ids=[
        "conftest-added",
        "pyproject-changed",
        "pyproject-deleted",
        "setup-cfg",
        "tox-ini",
        "package-json",
        "vitest-config",
        "nested-conftest",
    ],
)
def test_root_infrastructure_changed_after_test_commit_fails(repo, path, text):
    repo.declare()
    repo.fix({path: text})
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert protected_line(path) in out, out


def test_root_infrastructure_from_a_clean_base_merge_passes(repo):
    repo.declare()
    repo.fix()
    repo.g("checkout", "-q", "main")
    repo.commit("main: pytest.ini", {"pytest.ini": PYTEST_INI})
    repo.base = repo.rev("HEAD")
    repo.g("checkout", "-q", "feat/x")
    repo.g("merge", "-q", "--no-edit", "main")
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_root_infrastructure_in_test_commit_is_refused(repo):
    repo.declare({"pytest.ini": PYTEST_INI})
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out
    assert "test commit changes pytest.ini, which is neither declared nor a test helper" in out, out
