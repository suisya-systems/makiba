"""Acceptance test for Issue 514 (docs/design/helper-fix.md, option D, the checker side).

scripts/acceptance-check.sh, rule 4: a test helper the test commit left alone may change, be added
or be deleted by the own commits after it, and each such helper is named before the last line. The
declared paths, every path the test commit changed, .github/, the checker and the helpers whose
file name is in the gate's test-infrastructure list (src/daemon/gate.rs INFRA_NAMES,
INFRA_PREFIXES) stay protected. A base merge counts a file only as its own resolution: a file git
merged cleanly counts when the merge commit holds it otherwise than `git merge-tree --write-tree`
of its parents; a conflicted file counts unless the merge kept the branch's version.

Each case builds a scratch repository (a base on main, a test commit declaring tests/test_new.py
on feat/x, a fix) and runs the checker on it, as tests/test_acceptance_check.py does. The helpers
live under tests/common/, which is a helper by place whether or not Issue 372 has landed."""

import os
import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "acceptance-check.sh"
GATE = ROOT / "src" / "daemon" / "gate.rs"

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
HELPER = "def util():\n    return 1\n"
NL_HELPER = "tests/common/a\nb"

# A declared test the base already holds (cases 6 and 7): the test commit changes its assertion,
# main changes its last line, far enough apart for git to merge them cleanly.
TAIL = "    # a\n    # b\n    # c\n    # d\n    # e\n"
T_BASE = "from lib import answer\n\n\ndef test_answer():\n    assert answer() >= 1\n" + TAIL + "    # end\n"
T_M = T_BASE.replace("answer() >= 1", "answer() == 2")
T_MAIN = T_BASE.replace("# end", "# end of main")
T_CLEAN = T_M.replace("# end", "# end of main")
# main changing the assertion line itself: a conflict with the test commit.
T_MAIN_CONFLICT = T_BASE.replace("answer() >= 1", "answer() >= 0")


class Repo:
    """A scratch repository: base on main (lib.py answers 1, a workflow, test infrastructure,
    plain helpers under tests/common/), then a checked-out branch feat/x."""

    def __init__(self, path: Path, extra: dict[str, str] | None = None):
        self.path = path
        path.mkdir()
        self.g("init", "-q", "-b", "main")
        self.write("lib.py", "def answer():\n    return 1\n")
        self.write(".github/workflows/ci.yml", "name: CI\n")
        self.write("tests/conftest.py", "# helper\n")
        self.write("tests/common/conftest.py", "# helper\n")
        self.write("tests/common/helper_util.py", HELPER)
        self.write("tests/common/a", "a\n")
        for rel, text in (extra or {}).items():
            self.write(rel, text)
        self.g("add", "-A")
        self.g("commit", "-q", "-m", "base")
        self.base = self.rev("HEAD")
        self.g("checkout", "-q", "-b", "feat/x")

    def run_git(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["git", "-c", "commit.gpgsign=false", *args],
            cwd=self.path,
            env=ENV,
            check=False,
            capture_output=True,
            text=True,
        )

    def g(self, *args: str) -> str:
        r = self.run_git(*args)
        assert r.returncode == 0, f"git {args}: {r.stdout}{r.stderr}"
        return r.stdout

    def rev(self, ref: str) -> str:
        return self.g("rev-parse", ref).strip()

    def write(self, rel: str, text: str) -> None:
        f = self.path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")

    def read(self, rel: str) -> str:
        return (self.path / rel).read_text(encoding="utf-8")

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
        """The test commit: tests/test_new.py (TEST_BODY unless given) and any other files,
        declaring tests/test_new.py alone."""
        files = dict(files or {})
        files.setdefault("tests/test_new.py", TEST_BODY)
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
        self.commit("fix", {"lib.py": "def answer():\n    return 2\n", **(files or {})})

    def main_commit(self, rel: str, text: str) -> None:
        self.g("checkout", "-q", "main")
        self.commit(f"main: {rel}", {rel: text})
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


def helper_line(path: str) -> str:
    return (
        f"acceptance-check: test helper {path} changed after the test commit;"
        " the person accepts it at the merge"
    )


@pytest.fixture
def repo(tmp_path):
    return Repo(tmp_path / "r")


@pytest.fixture
def merged(tmp_path):
    """Cases 6 and 7: the base holds tests/test_new.py, the test commit changes its assertion."""
    r = Repo(tmp_path / "r", {"tests/test_new.py": T_BASE})
    r.declare({"tests/test_new.py": T_M})
    return r


# 1-2: a helper the test commit left alone


def test_untouched_helper_changed_by_fix_passes(repo):
    repo.declare()
    repo.fix({"tests/common/helper_util.py": "def util():\n    return 2\n"})
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert helper_line("tests/common/helper_util.py") in out.splitlines(), out


def test_helper_added_by_fix_passes(repo):
    repo.declare()
    repo.fix({"tests/common/helper_new.py": HELPER})
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert helper_line("tests/common/helper_new.py") in out.splitlines(), out


def test_helper_deleted_by_fix_passes(repo):
    repo.declare()
    repo.fix({"tests/common/helper_util.py": None})
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert helper_line("tests/common/helper_util.py") in out.splitlines(), out


# 3-5: what stays protected


@pytest.mark.parametrize("path", ["tests/conftest.py", "tests/common/conftest.py"])
def test_infrastructure_helper_changed_by_fix_fails(repo, path):
    repo.declare()
    repo.fix()
    repo.commit("infra", {path: "# changed\n"})
    rc, out = repo.check()
    assert rc == 1, out
    assert f"{path} changed after the test commit" in out, out
    assert f"test helper {path}" not in out, out


def test_helper_changed_by_test_commit_then_by_fix_fails(repo):
    repo.declare({"tests/common/helper_util.py": "def util():\n    return 3\n"})
    repo.fix({"tests/common/helper_util.py": "def util():\n    return 4\n"})
    rc, out = repo.check()
    assert rc == 1, out
    assert "tests/common/helper_util.py changed after the test commit" in out, out
    assert "test helper tests/common/helper_util.py" not in out, out


@pytest.mark.parametrize(
    "path", ["tests/test_new.py", ".github/workflows/ci.yml", "scripts/acceptance-check.sh"]
)
def test_protected_path_changed_by_fix_fails(repo, path):
    repo.declare()
    repo.fix()
    repo.commit("protected", {path: TEST_BODY + "# changed\n"})
    rc, out = repo.check()
    assert rc == 1, out
    assert f"{path} changed after the test commit" in out, out


# 6-7: base merges


def test_clean_base_merge_into_declared_test_passes(merged):
    merged.main_commit("tests/test_new.py", T_MAIN)
    merged.g("merge", "-q", "--no-ff", "-m", "merge main", "main")
    assert merged.read("tests/test_new.py") == T_CLEAN
    merged.fix()
    rc, out = merged.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_conflicted_merge_keeping_the_branch_version_passes(merged):
    merged.main_commit("tests/test_new.py", T_MAIN_CONFLICT)
    assert merged.run_git("merge", "-q", "--no-ff", "-m", "merge main", "main").returncode != 0
    merged.g("checkout", "--ours", "--", "tests/test_new.py")
    merged.g("add", "tests/test_new.py")
    merged.g("commit", "-q", "-m", "merge main")
    merged.fix()
    rc, out = merged.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out


def test_merge_resolving_declared_test_otherwise_fails(merged):
    merged.main_commit("tests/test_new.py", T_MAIN)
    merged.g("merge", "-q", "--no-ff", "--no-commit", "main")
    merged.write("tests/test_new.py", T_CLEAN + "    # own\n")
    merged.g("add", "tests/test_new.py")
    merged.g("commit", "-q", "-m", "merge main")
    merged.fix()
    rc, out = merged.check()
    assert rc == 1, out
    assert "tests/test_new.py changed after the test commit" in out, out


def test_clean_merge_setting_declared_test_back_to_main_fails(merged):
    merged.main_commit("tests/test_new.py", T_MAIN)
    merged.g("merge", "-q", "--no-ff", "--no-commit", "main")
    merged.write("tests/test_new.py", T_MAIN)
    merged.g("add", "tests/test_new.py")
    merged.g("commit", "-q", "-m", "merge main")
    merged.fix()
    rc, out = merged.check()
    assert rc == 1, out
    assert "tests/test_new.py changed after the test commit" in out, out


def test_conflicted_merge_taking_main_version_fails(merged):
    merged.main_commit("tests/test_new.py", T_MAIN_CONFLICT)
    assert merged.run_git("merge", "-q", "--no-ff", "-m", "merge main", "main").returncode != 0
    merged.g("checkout", "--theirs", "--", "tests/test_new.py")
    merged.g("add", "tests/test_new.py")
    merged.g("commit", "-q", "-m", "merge main")
    merged.fix()
    rc, out = merged.check()
    assert rc == 1, out
    assert "tests/test_new.py changed after the test commit" in out, out


# 8: one infrastructure list for the checker and the gate


def gate_list(text: str, name: str) -> list[str]:
    m = re.search(r"const " + name + r": &\[&str\] = &\[(.*?)\];", text, re.S)
    assert m, f"{name} not found in {GATE}"
    return re.findall(r'"([^"]*)"', m.group(1))


def script_list(text: str, name: str) -> list[str]:
    m = re.search(r"^" + name + r"='([^']*)'", text, re.M)
    assert m, f"{name}='...' not found in {SCRIPT}"
    return m.group(1).split()


@pytest.mark.parametrize("name", ["INFRA_NAMES", "INFRA_PREFIXES"])
def test_infrastructure_list_matches_the_gate(name):
    gate = gate_list(GATE.read_text(encoding="utf-8"), name)
    script = script_list(SCRIPT.read_text(encoding="utf-8"), name)
    assert gate and script, (gate, script)
    assert sorted(script) == sorted(gate)


# 9: names are read whole (a newline in a helper's name)


def test_helper_with_newline_in_test_commit_does_not_protect_its_prefix(repo):
    repo.declare({NL_HELPER: "x\n"})
    repo.fix({"tests/common/a": "a2\n"})
    rc, out = repo.check()
    assert (rc, last(out)) == (0, "ACCEPTANCE: pass"), out
    assert helper_line("tests/common/a") in out.splitlines(), out


def test_helper_with_newline_changed_by_test_commit_stays_protected(repo):
    repo.declare({NL_HELPER: "x\n"})
    repo.fix({NL_HELPER: "y\n"})
    rc, out = repo.check()
    assert rc == 1, out
    assert "changed after the test commit (" in out, out
