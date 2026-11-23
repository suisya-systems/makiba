"""Every tracked shell script is linted, the list taken from git itself (Issue 354).

tools/shell_lint.py runs `bash -n` and `shellcheck --severity=error` on each file of
`git ls-files -z -- '*.sh'`, so no glob list has to follow the tree. Needs bash and
shellcheck on PATH (ubuntu-latest has both); without shellcheck these tests fail."""
import importlib.util
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("shell_lint", ROOT / "tools" / "shell_lint.py")
sl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sl)

BROKEN = "#!/bin/bash\nif then fi\n"
CLEAN = "#!/bin/bash\necho ok\n"
# Parses, but shellcheck reports SC2218 (a function used before its definition) as an error.
SC_ERROR = "#!/bin/bash\nf\nf() { :; }\n"


def git(root, *args):
    subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)


def scratch_repo(tmp_path, files, tracked=None):
    """A git repository at tmp_path/repo holding files ({path: text}); the paths of tracked
    (all of files when None) are added to the index."""
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "test@example.com")
    git(repo, "config", "user.name", "test")
    for path, text in files.items():
        p = repo / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
    for path in files if tracked is None else tracked:
        git(repo, "add", "--", path)
    return repo


def test_repository_scripts_all_pass():
    assert sl.lint(ROOT) == []
    assert {
        "scripts/acceptance-check.sh",
        "scripts/probes/lib.sh",
        "scripts/runbooks/self-hosting.sh",
    } <= set(sl.tracked_scripts(ROOT))


def test_syntax_error_in_any_folder_is_named(tmp_path):
    repo = scratch_repo(tmp_path, {"tools/deep/x.sh": BROKEN})
    fails = sl.lint(repo)
    assert len(fails) == 1
    assert fails[0].startswith("tools/deep/x.sh: bash -n")


def test_shellcheck_error_is_named(tmp_path):
    repo = scratch_repo(tmp_path, {"a/b.sh": SC_ERROR})
    fails = sl.lint(repo)
    assert len(fails) == 1
    assert fails[0].startswith("a/b.sh: shellcheck")
    assert "SC2218" in fails[0]


def test_untracked_script_is_not_linted_until_added(tmp_path):
    repo = scratch_repo(tmp_path, {"new.sh": BROKEN}, tracked=[])
    assert sl.lint(repo) == []
    assert "new.sh" not in sl.tracked_scripts(repo)
    git(repo, "add", "new.sh")
    fails = sl.lint(repo)
    assert len(fails) == 1
    assert fails[0].startswith("new.sh: bash -n")


def test_unusual_names_are_linted(tmp_path):
    names = [".hidden.sh", "dir/name with space.sh", "-dash.sh"]
    repo = scratch_repo(tmp_path, {n: BROKEN for n in names})
    fails = sl.lint(repo)
    assert sorted(f.split(": ")[0] for f in fails) == sorted(names)


def test_clean_scripts_pass(tmp_path):
    repo = scratch_repo(tmp_path, {"ok.sh": CLEAN})
    assert sl.lint(repo) == []


def test_missing_shellcheck_is_an_error(tmp_path):
    repo = scratch_repo(tmp_path, {"ok.sh": CLEAN})
    with pytest.raises(RuntimeError, match="shellcheck"):
        sl.lint(repo, shellcheck="/nonexistent/shellcheck")
    assert sl.main([str(repo)], shellcheck="/nonexistent/shellcheck") == 2


def test_main_exit_codes(tmp_path, capsys):
    repo = scratch_repo(tmp_path, {"s/x.sh": BROKEN})
    assert sl.main([str(repo)]) == 1
    assert "s/x.sh" in capsys.readouterr().out
    (repo / "s" / "x.sh").write_text(CLEAN, encoding="utf-8")
    assert sl.main([str(repo)]) == 0
