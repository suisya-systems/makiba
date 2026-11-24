"""Forms of the sites tools/check_test_executables.py must not miss (Issue 379, diff review):
a mode argument with parentheses in it, chmod named by path, and fs::copy brought in by a `use`.
The declared test (tests/test_check_test_executables.py) holds the rest."""
import importlib.util
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_test_executables", ROOT / "tools" / "check_test_executables.py")
ce = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ce)


def lines(text):
    return [int(f.split(":")[1]) for f in ce.check_text("tests/x.rs", text)]


@pytest.mark.parametrize("arg", ["(0o755)", "((0o700))", "mode()", "m(1)", "f(a, (b))", "( 0o7_55 )"])
def test_a_mode_argument_with_parentheses_is_reported(arg):
    assert lines(f"set_permissions(&p, Permissions::from_mode({arg})).unwrap();\n") == [1]
    assert lines(f"perms.set_mode({arg});\n") == [1]


@pytest.mark.parametrize("arg", ["(0o644)", "((0o600))"])
def test_a_parenthesized_literal_without_an_execute_bit_is_not_reported(arg):
    assert lines(f"set_permissions(&p, Permissions::from_mode({arg})).unwrap();\n") == []


def test_a_mode_argument_split_over_lines_with_a_call_is_reported_at_its_first_line():
    assert lines("let p = Permissions::from_mode(\n    mode_of(\n        &q,\n    ),\n);\n") == [1]


def test_an_unclosed_mode_call_is_reported():
    findings = ce.check_text("tests/x.rs", "let p = Permissions::from_mode(\n" + "x\n" * 100)
    assert len(findings) == 1 and len(findings[0]) < 400


def test_the_getter_with_parentheses_inside_the_call_chain_is_not_reported():
    assert lines("let m = fs::metadata(&p).unwrap().permissions().mode() & 0o777;\n") == []


@pytest.mark.parametrize("name", ['"/bin/chmod"', '"/usr/bin/chmod"', '"chmod"', ' "chmod" '])
def test_chmod_by_name_or_path_is_reported(name):
    assert lines(f"let s = Command::new({name}).arg(&p).status();\n") == [1]


def test_a_program_whose_name_only_ends_in_chmod_like_text_is_not_chmod():
    assert lines('let s = Command::new("mychmod").status();\n') == []


@pytest.mark.parametrize("text", [
    "use std::fs::copy;\n",
    "use std::fs::{self, copy};\n",
    "use std::fs::{\n    copy,\n    write,\n};\n",
    "let f = std::fs::copy;\n",
])
def test_fs_copy_brought_in_or_named_is_reported(text):
    assert lines(text) == [1]


def test_a_copy_that_is_not_fs_copy_is_not_reported():
    assert lines("let v = a.copy_from_slice(&b);\nuse std::fs::{self, write};\n") == []
