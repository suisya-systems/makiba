"""Acceptance test for `scripts/acceptance-check.sh --scope BASE HEAD` (Issue 480, build 1 of
docs/design/test-commit-scoped-ci.md, section 3.1): it says whether a head is its test commit
alone, and which tests that commit declares. Each case builds a scratch repository as
tests/test_acceptance_check.py does and reads the mode's output: `test-commit <sha>` then one
`rs <target name>` or `py <path>` line per declared test, or a single `full: <reason>` line;
exit 0 in both cases, 2 on a usage or git error."""

import subprocess

import pytest

from test_acceptance_check import ENV, SCRIPT, Repo


@pytest.fixture
def repo(tmp_path):
    return Repo(tmp_path / "r")


def scope(repo, *args):
    r = subprocess.run(
        ["bash", str(SCRIPT), "--scope", *args],
        cwd=repo.path,
        env=ENV,
        check=False,
        capture_output=True,
        text=True,
    )
    return r.returncode, r.stdout, r.stderr


def scope_head(repo):
    return scope(repo, repo.base, repo.rev("HEAD"))


def assert_test_commit(repo, *tests):
    rc, out, err = scope_head(repo)
    assert rc == 0, (out, err)
    assert out.splitlines() == [f"test-commit {repo.rev('HEAD')}", *tests], (out, err)


def assert_full(repo):
    rc, out, err = scope_head(repo)
    assert rc == 0, (out, err)
    lines = out.splitlines()
    assert len(lines) == 1 and lines[0].startswith("full: ") and len(lines[0]) > len("full: "), (
        out,
        err,
    )


# test-commit: the head is one commit, its test commit alone


def test_rust_file_target(repo):
    repo.declare_rs("tests/answer.rs")
    assert_test_commit(repo, "rs answer")


def test_rust_folder_target(repo):
    repo.declare_rs("tests/answer/main.rs")
    assert_test_commit(repo, "rs answer")


def test_python_test(repo):
    repo.declare("tests/test_new.py")
    assert_test_commit(repo, "py tests/test_new.py")


def test_declared_directory(repo):
    repo.add_test("tests/acc/test_a.py")
    repo.add_test("tests/acc/test_b.py")
    repo.write("tests/acc/data.txt", "fixture\n")
    repo.g("add", "tests/acc")
    repo.test_commit("tests/acc")
    assert_test_commit(repo, "py tests/acc/test_a.py", "py tests/acc/test_b.py")


def test_target_with_helper(repo):
    repo.write("tests/data.txt", "fixture\n")
    repo.write("tests/answer.rs", "#[test]\nfn answer_is_two() {\n    assert_eq!(makiba::answer(), 2);\n}\n")
    repo.g("add", "tests/data.txt", "tests/answer.rs")
    repo.test_commit("tests/answer.rs", "tests/data.txt")
    assert_test_commit(repo, "rs answer")


# full: anything else


def test_two_own_commits(repo):
    repo.declare("tests/test_new.py")
    repo.fix()
    assert_full(repo)


def test_base_merge_after_test_commit(repo):
    repo.declare("tests/test_new.py")
    repo.main_commit("other.txt", "x\n")
    repo.g("merge", "-q", "--no-ff", "-m", "merge main", "main")
    assert_full(repo)


def test_merge_as_test_commit(repo):
    repo.main_commit("other.txt", "x\n")
    repo.g("merge", "-q", "--no-ff", "--no-commit", "main")
    repo.add_test("tests/test_new.py")
    repo.g("add", "tests/test_new.py")
    repo.test_commit("tests/test_new.py")
    assert len(repo.g("rev-list", "--first-parent", "HEAD", "--not", repo.base).split()) == 1
    assert_full(repo)


def test_test_commit_also_changing_code(repo):
    repo.add_test("tests/test_new.py")
    repo.write("lib.py", "def answer():\n    return 2\n")
    repo.g("add", "tests/test_new.py", "lib.py")
    repo.test_commit("tests/test_new.py")
    assert_full(repo)


def test_only_helpers_declared(repo):
    repo.declare_rs("tests/common/mod.rs", "pub fn helper() {}\n")
    assert_full(repo)


def test_missing_declared_path(repo):
    repo.add_test("tests/test_new.py")
    repo.g("add", "tests/test_new.py")
    repo.test_commit("tests/test_new.py", "tests/test_missing.py")
    assert_full(repo)


def test_no_declaration(repo):
    repo.add_test("tests/test_new.py")
    repo.g("add", "tests/test_new.py")
    repo.g("commit", "-q", "-m", "test: no trailer")
    assert_full(repo)


# exit 2: usage and git errors


@pytest.mark.parametrize("args", [(), ("a",), ("a", "b", "c")])
def test_bad_arguments_exit_2(repo, args):
    rc, out, err = scope(repo, *args)
    assert rc == 2, (out, err)


def test_unknown_head_exits_2(repo):
    repo.declare("tests/test_new.py")
    rc, out, err = scope(repo, repo.base, "0" * 40)
    assert rc == 2, (out, err)


def test_unknown_base_exits_2(repo):
    repo.declare("tests/test_new.py")
    rc, out, err = scope(repo, "no-such-ref", repo.rev("HEAD"))
    assert rc == 2, (out, err)
