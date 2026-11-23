"""Every tracked shell script is covered by the lint job's shell step (Issue 302)."""
import importlib.util
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("shell_lint_coverage", ROOT / "tools" / "shell_lint_coverage.py")
cl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cl)

CI = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")


def SYN(loop, sc):
    return (
        "jobs:\n"
        "  lint:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - name: Shell scripts parse, and shellcheck finds no error\n"
        "        run: |\n"
        "          shellcheck --version\n"
        f"          for f in {loop}; do bash -n \"$f\" || exit 1; done\n"
        f"          shellcheck --severity=error {sc}\n"
    )


def git(root, *args):
    subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)


def test_repository_scripts_are_all_linted():
    assert cl.uncovered(CI, cl.tracked_scripts(ROOT)) == []


def test_script_in_new_scripts_folder_is_reported():
    assert cl.uncovered(CI, cl.tracked_scripts(ROOT) + ["scripts/newfolder/x.sh"]) == ["scripts/newfolder/x.sh"]


def test_script_outside_scripts_is_reported():
    assert cl.uncovered(CI, cl.tracked_scripts(ROOT) + ["tools/x.sh"]) == ["tools/x.sh"]


def test_glob_does_not_cross_folders():
    assert cl.uncovered(SYN("scripts/*.sh", "scripts/*.sh"), ["scripts/a.sh", "scripts/probes/b.sh"]) == [
        "scripts/probes/b.sh"
    ]


def test_file_left_out_of_one_line_is_reported():
    assert cl.uncovered(
        SYN("scripts/*.sh scripts/probes/*.sh", "scripts/*.sh"), ["scripts/a.sh", "scripts/probes/b.sh"]
    ) == ["scripts/probes/b.sh"]


def test_missing_step_raises():
    with pytest.raises(ValueError):
        cl.uncovered("jobs: {}\n", ["a.sh"])


def test_main_names_left_out_file(tmp_path, capsys):
    git(tmp_path, "init", "-q")
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    shutil.copy(ROOT / ".github" / "workflows" / "ci.yml", tmp_path / ".github" / "workflows" / "ci.yml")
    for rel in cl.tracked_scripts(ROOT):
        (tmp_path / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / rel, tmp_path / rel)
    (tmp_path / "scripts" / "newfolder").mkdir(parents=True)
    (tmp_path / "scripts" / "newfolder" / "x.sh").write_text("#!/usr/bin/env bash\necho x\n", encoding="utf-8")
    git(tmp_path, "add", "-A")
    assert cl.main([str(tmp_path)]) == 1
    assert "scripts/newfolder/x.sh" in capsys.readouterr().out
    git(tmp_path, "rm", "-q", "--cached", "scripts/newfolder/x.sh")
    assert cl.main([str(tmp_path)]) == 0


def test_tracked_scripts_reads_unusual_names(tmp_path):
    git(tmp_path, "init", "-q")
    name = "scripts/é x.sh"
    (tmp_path / "scripts").mkdir()
    (tmp_path / name).write_text("#!/usr/bin/env bash\n", encoding="utf-8")
    git(tmp_path, "add", "-A")
    assert cl.tracked_scripts(tmp_path) == [name]
