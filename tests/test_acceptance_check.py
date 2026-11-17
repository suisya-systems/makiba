"""Acceptance test for scripts/acceptance-check.sh, the CI job that says whether a declared
test failed at the test commit and passed at head (Issue #3, carried from paddock's
tests/acceptance-check.test.sh). Each case builds a scratch git repository with a base,
a test commit and a fix, and runs the checker on it."""

import os
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "acceptance-check.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
REPO_CHECK = ROOT / ".github" / "acceptance-check"

ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@t",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@t",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}
TEST_BODY = "from lib import answer\n\n\ndef test_answer():\n    assert answer() == 2\n"


class Repo:
    """A scratch repository: base on main (lib.py answers 1, a workflow, a test helper),
    then a checked-out branch feat/x."""

    def __init__(self, path: Path):
        self.path = path
        path.mkdir()
        self.g("init", "-q", "-b", "main")
        self.write("lib.py", "def answer():\n    return 1\n")
        self.write(".github/workflows/ci.yml", "name: CI\n")
        self.write("tests/conftest.py", "# helper\n")
        self.g("add", "-A")
        self.g("commit", "-q", "-m", "base")
        self.base = self.rev("HEAD")
        self.g("checkout", "-q", "-b", "feat/x")

    def g(self, *args: str) -> str:
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", *args],
            cwd=self.path,
            env=ENV,
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def rev(self, ref: str) -> str:
        return self.g("rev-parse", ref).strip()

    def write(self, rel: str, text: str) -> None:
        f = self.path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")

    def add_test(self, rel: str) -> None:
        """A test that passes only once lib.py answers 2."""
        self.write(rel, TEST_BODY)

    def fix(self) -> None:
        self.write("lib.py", "def answer():\n    return 2\n")
        self.g("commit", "-q", "-am", "fix")

    def test_commit(self, *paths: str) -> None:
        """Commit what is staged with one trailer line per path."""
        trailers = "".join(f"Acceptance-Test: {p}\n" for p in paths)
        self.g("commit", "-q", "-m", f"test: declare\n\n{trailers}Co-Authored-By: t <t@t>")

    def declare(self, *paths: str) -> None:
        """add_test for each path, stage it, and make the test commit."""
        for p in paths:
            self.add_test(p)
            self.g("add", p)
        self.test_commit(*paths)

    def main_commit(self, rel: str, text: str) -> None:
        self.g("checkout", "-q", "main")
        self.write(rel, text)
        self.g("add", rel)
        self.g("commit", "-q", "-m", f"main: {rel}")
        self.base = self.rev("HEAD")
        self.g("checkout", "-q", "feat/x")

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


@pytest.fixture
def repo(tmp_path):
    return Repo(tmp_path / "r")


def test_script_exists():
    assert SCRIPT.is_file()


def test_no_arguments_exits_2(tmp_path):
    r = subprocess.run(
        ["bash", str(SCRIPT)], cwd=tmp_path, check=False, capture_output=True, text=True
    )
    assert r.returncode == 2


def test_fails_at_test_commit_passes_at_head(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_no_trailer_is_none(repo):
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: none"), out


def test_declaration_split_from_trailer_block_is_not_a_trailer(repo):
    repo.add_test("tests/test_new.py")
    repo.g("add", "tests/test_new.py")
    repo.g(
        "commit",
        "-q",
        "-m",
        "test: declare\n\nAcceptance-Test: tests/test_new.py\n\nCo-Authored-By: t <t@t>",
    )
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: none"), out


def test_already_passes_at_test_commit_is_fail(repo):
    repo.write("lib.py", "def answer():\n    return 2\n")
    repo.g("commit", "-q", "-am", "early")
    repo.base = repo.rev("HEAD")
    repo.declare("tests/test_new.py")
    repo.g("commit", "-q", "--allow-empty", "-m", "noop")
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_still_fails_at_head_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.g("commit", "-q", "--allow-empty", "-m", "nofix")
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_declared_test_changed_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.write("tests/test_new.py", "def test_answer():\n    pass\n")
    repo.g("commit", "-q", "-am", "weaken")
    rc, out = repo.check()
    assert rc == 1 and "tests/test_new.py" in out, out


def test_workflow_changed_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.write(".github/workflows/ci.yml", "name: other\n")
    repo.g("commit", "-q", "-am", "ci")
    rc, out = repo.check()
    assert rc == 1 and ".github/workflows/ci.yml" in out, out


def test_helper_changed_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.write("tests/conftest.py", "# changed\n")
    repo.g("commit", "-q", "-am", "helper")
    rc, out = repo.check()
    assert rc == 1 and "tests/conftest.py" in out, out


def test_checker_changed_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.write("scripts/acceptance-check.sh", "echo 'ACCEPTANCE: pass'\n")
    repo.g("add", "scripts/acceptance-check.sh")
    repo.g("commit", "-q", "-m", "checker")
    rc, out = repo.check()
    assert rc == 1 and "scripts/acceptance-check.sh" in out, out


def test_test_commit_also_changing_code_is_fail(repo):
    repo.add_test("tests/test_new.py")
    repo.write("lib.py", "def answer():\n    return 2\n")
    repo.g("add", "tests/test_new.py", "lib.py")
    repo.test_commit("tests/test_new.py")
    rc, out = repo.check()
    assert rc == 1 and "lib.py" in out, out


def test_declaration_not_on_first_commit_is_fail(repo):
    repo.g("commit", "-q", "--allow-empty", "-m", "first")
    repo.declare("tests/test_new.py")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_declared_path_missing_is_fail(repo):
    repo.add_test("tests/test_new.py")
    repo.g("add", "tests/test_new.py")
    repo.test_commit("tests/test_new.py", "tests/test_missing.py")
    repo.fix()
    rc, out = repo.check()
    assert rc == 1 and "tests/test_missing.py" in out, out


def test_declared_directory_runs_every_test_file(repo):
    repo.add_test("tests/acc/test_a.py")
    repo.add_test("tests/acc/test_b.py")
    repo.write("tests/acc/data.txt", "fixture\n")
    repo.g("add", "tests/acc")
    repo.test_commit("tests/acc")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert "tests/acc/test_a.py" in out and "tests/acc/test_b.py" in out, out


def test_declared_directory_with_one_test_passing_at_test_commit_is_fail(repo):
    repo.add_test("tests/acc/test_a.py")
    repo.write("tests/acc/test_b.py", "def test_ok():\n    pass\n")
    repo.g("add", "tests/acc")
    repo.test_commit("tests/acc")
    repo.fix()
    rc, out = repo.check()
    assert rc == 1 and "tests/acc/test_b.py" in out, out


def test_declaration_with_no_test_to_run_is_fail(repo):
    repo.write("tests/data.txt", "fixture\n")
    repo.g("add", "tests/data.txt")
    repo.test_commit("tests/data.txt")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_declared_file_that_collects_no_test_at_test_commit_is_fail(repo):
    # Collects nothing before the fix (pytest exit 5): that is not a failing test.
    repo.write(
        "tests/test_late.py",
        "from lib import answer\n\nif answer() == 2:\n\n    def test_answer():\n        pass\n",
    )
    repo.g("add", "tests/test_late.py")
    repo.test_commit("tests/test_late.py")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_new_test_file_after_test_commit_is_allowed(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.add_test("tests/test_later.py")
    repo.g("add", "tests/test_later.py")
    repo.g("commit", "-q", "-m", "more tests")
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_helper_renamed_out_of_tests_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.g("mv", "tests/conftest.py", "helper.py")
    repo.g("commit", "-q", "-m", "move helper")
    rc, out = repo.check()
    assert rc == 1 and "tests/conftest.py changed after" in out, out


def test_code_renamed_into_declared_path_in_test_commit_is_fail(repo):
    repo.add_test("tests/test_new.py")
    (repo.path / "tests" / "new").mkdir()
    repo.g("mv", "lib.py", "tests/new/lib.py")
    repo.g("add", "tests/test_new.py")
    repo.test_commit("tests/test_new.py", "tests/new")
    rc, out = repo.check()
    assert rc == 1 and "test commit changes lib.py" in out, out


def test_base_merged_after_test_commit_is_pass(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.main_commit("other.txt", "x\n")
    repo.main_commit(".github/workflows/ci.yml", "name: CI2\n")
    repo.main_commit("tests/conftest.py", "# helper 2\n")
    repo.g("merge", "-q", "--no-ff", "-m", "merge main", "main")
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_merge_before_test_commit_is_fail(repo):
    repo.main_commit("other.txt", "x\n")
    repo.g("merge", "-q", "--no-ff", "-m", "merge main", "main")
    repo.declare("tests/test_new.py")
    repo.fix()
    rc, out = repo.check()
    assert (rc, last(out)) == (1, "ACCEPTANCE: fail"), out


def test_merge_changing_protected_path_beyond_base_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.main_commit("other.txt", "x\n")
    repo.g("merge", "-q", "--no-ff", "--no-commit", "main")
    repo.write("tests/conftest.py", "# own\n")
    repo.g("add", "tests/conftest.py")
    repo.g("commit", "-q", "-m", "merge main")
    rc, out = repo.check()
    assert rc == 1 and "tests/conftest.py" in out, out


def test_merge_of_non_base_branch_after_test_commit_is_fail(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    repo.g("checkout", "-q", "-b", "side")
    repo.write("tests/test_new.py", "def test_answer():\n    pass\n")
    repo.g("commit", "-q", "-am", "weaken")
    repo.g("checkout", "-q", "feat/x")
    repo.g("merge", "-q", "--no-ff", "-m", "merge side", "side")
    rc, out = repo.check()
    assert rc == 1 and "not in the base" in out, out


def test_ci_has_tests_lint_and_acceptance_jobs():
    text = WORKFLOW.read_text(encoding="utf-8")
    for job in ("tests", "lint", "acceptance"):
        assert f"\n  {job}:\n" in text, job
    assert "scripts/acceptance-check.sh" in text
    assert "fetch-depth: 0" in text


def test_repo_check_names_the_acceptance_job():
    lines = [ln for ln in REPO_CHECK.read_text(encoding="utf-8").splitlines() if ln.strip()]
    assert lines == ["acceptance"]
    assert f"\n  {lines[0]}:\n" in WORKFLOW.read_text(encoding="utf-8")
