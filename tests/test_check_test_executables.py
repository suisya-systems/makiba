"""No test makes an executable in its own process (Issues 374, 379).

A file this process writes and then runs can fail to exec with ETXTBSY ("Text file busy") when
another test thread forks while the write descriptor is open. tests/common/exe.rs makes every
executable in a `cp` child; tools/check_test_executables.py reports every other place under
tests/ that sets an executable mode, copies a file with fs::copy or runs chmod, unless a comment
`// not an executable: <reason>` on that line or the line above says it makes no executable."""
import importlib.util
import re
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "tools" / "check_test_executables.py"
spec = importlib.util.spec_from_file_location("check_test_executables", TOOL)
ce = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ce)


def lines(findings, name="tests/x.rs"):
    """The line numbers the findings name, in order."""
    out = []
    for f in findings:
        m = re.match(re.escape(name) + r":(\d+): ", f)
        assert m, f"a finding does not start with {name}:<line>: {f!r}"
        out.append(int(m.group(1)))
    return out


def one(text):
    return lines(ce.check_text("tests/x.rs", text))


def test_a_write_then_an_executable_mode_is_reported_at_its_line():
    text = (
        "std::fs::write(&p, b).unwrap();\n"
        "std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();\n"
    )
    findings = ce.check_text("tests/x.rs", text)
    assert lines(findings) == [2]
    assert "common::exe" in findings[0]
    assert "not an executable:" in findings[0]


def test_a_mode_split_by_rustfmt_is_reported_at_the_from_mode_line():
    text = (
        "fn f(p: &Path) {\n"
        "    std::fs::set_permissions(\n"
        "        p,\n"
        "        std::fs::Permissions::from_mode(\n"
        "            0o755,\n"
        "        ),\n"
        "    )\n"
        "    .unwrap();\n"
        "}\n"
    )
    assert one(text) == [4]


@pytest.mark.parametrize("mode", ["0o700", "0o750", "0o701", "0o4755", "0o100", "0o0755", "0o7_55"])
def test_a_mode_with_an_execute_bit_is_reported(mode):
    assert one(f"set_permissions(&p, Permissions::from_mode({mode})).unwrap();\n") == [1]


@pytest.mark.parametrize("mode", ["0o644", "0o600", "0o444", "0o000", "0o4644"])
def test_a_mode_without_an_execute_bit_is_not_reported(mode):
    assert one(f"set_permissions(&p, Permissions::from_mode({mode})).unwrap();\n") == []


def test_a_mode_that_is_not_a_literal_is_reported():
    assert one("set_permissions(&p, Permissions::from_mode(mode)).unwrap();\n") == [1]
    assert one("set_permissions(&p, Permissions::from_mode(0o700 | x)).unwrap();\n") == [1]


def test_open_options_and_set_mode_are_read_too():
    assert one("let f = OpenOptions::new().write(true).mode(0o755).open(&p);\n") == [1]
    assert one("perms.set_mode(0o700);\n") == [1]
    assert one("let f = OpenOptions::new().write(true).mode(0o600).open(&p);\n") == []
    assert one("let m = fs::metadata(&p).unwrap().permissions().mode() & 0o777;\n") == []


def test_an_in_process_copy_is_reported():
    assert one("std::fs::copy(a, b).unwrap();\n") == [1]
    assert one("fs::copy(a, b).unwrap();\n") == [1]


def test_a_chmod_child_is_reported():
    assert one('let s = Command::new("chmod").arg("+x").arg(&p).status();\n') == [1]


def test_the_marker_exempts_its_line_and_the_line_below_it_only():
    site = "set_permissions(&d, Permissions::from_mode(0o700)).unwrap();"
    assert one(f"{site} // not an executable: a directory\n") == []
    assert one(f"// not an executable: a directory\n{site}\n") == []
    assert one(f"// not an executable: a directory\nlet x = 1;\n{site}\n") == [3]
    assert one(f"{site} // not an executable:\n") == [1]
    assert one(f"// not an executable:   \n{site}\n") == [2]


def test_every_site_of_a_file_is_reported():
    text = (
        "fs::write(&a, x).unwrap();\n"
        "set_permissions(&a, Permissions::from_mode(0o755)).unwrap();\n"
        "fs::copy(&a, &b).unwrap();\n"
        "set_permissions(&d, Permissions::from_mode(0o700)).unwrap(); // not an executable: a dir\n"
        "let y = 2;\n"
        "set_permissions(&c, Permissions::from_mode(0o755)).unwrap();\n"
    )
    assert one(text) == [2, 3, 6]


SITE = "set_permissions(&p, Permissions::from_mode(0o755)).unwrap();\n"


def tree(root, files):
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")


def test_the_tree_is_read_below_tests_except_the_helper(tmp_path):
    tree(tmp_path, {
        "tests/a.rs": SITE,
        "tests/sub/b.rs": "let y = 2;\n" + SITE,
        "tests/common/exe.rs": SITE,
        "tests/x.txt": SITE,
        "src/lib.rs": SITE,
    })
    findings = ce.check(tmp_path)
    assert sorted(f.split(": ")[0] for f in findings) == ["tests/a.rs:1", "tests/sub/b.rs:2"]


def test_the_command_line_exits_1_with_findings_and_0_without(tmp_path):
    dirty, clean = tmp_path / "dirty", tmp_path / "clean"
    tree(dirty, {"tests/a.rs": SITE})
    tree(clean, {"tests/a.rs": "let y = 2;\n", "tests/common/exe.rs": SITE})
    r = subprocess.run([sys.executable, str(TOOL), str(dirty)], capture_output=True, text=True)
    assert r.returncode == 1, r.stdout + r.stderr
    assert "tests/a.rs:1" in r.stdout
    r = subprocess.run([sys.executable, str(TOOL), str(clean)], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr


def test_the_repository_makes_no_executable_in_process():
    assert ce.check(ROOT) == []


def test_the_helper_the_findings_name_exists_and_uses_a_cp_child():
    exe = (ROOT / "tests/common/exe.rs").read_text(encoding="utf-8")
    assert 'Command::new("cp")' in exe
    assert "fs::write(path" not in exe
    mod = (ROOT / "tests/common/mod.rs").read_text(encoding="utf-8")
    assert re.search(r"^pub mod exe;$", mod, re.M)
