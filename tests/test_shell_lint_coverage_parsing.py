"""How tools/shell_lint_coverage.py reads globs as bash does (Issue 302, review of the fix)."""
import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("shell_lint_coverage", ROOT / "tools" / "shell_lint_coverage.py")
cl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cl)


def SYN(loop, sc):
    return (
        "jobs:\n"
        "  lint:\n"
        "    steps:\n"
        "      - name: Shell scripts parse, and shellcheck finds no error\n"
        "        run: |\n"
        f"          for f in {loop}; do bash -n \"$f\" || exit 1; done\n"
        f"          shellcheck --severity=error {sc}\n"
    )


def test_wildcard_does_not_match_a_leading_dot():
    assert cl.uncovered(SYN("scripts/*.sh", "scripts/*.sh"), ["scripts/.hidden.sh", "scripts/a.sh"]) == [
        "scripts/.hidden.sh"
    ]
    assert cl.uncovered(SYN("scripts/*.sh", "scripts/*.sh"), [".ci/a.sh"]) == [".ci/a.sh"]


def test_explicit_leading_dot_matches():
    assert cl.uncovered(SYN("scripts/.*.sh", "scripts/.*.sh"), ["scripts/.hidden.sh"]) == []


def test_comment_is_not_coverage():
    assert cl.uncovered(SYN("scripts/*.sh tools/*.sh", "scripts/*.sh # tools/*.sh"), ["tools/x.sh"]) == ["tools/x.sh"]
    assert cl.uncovered(SYN("scripts/*.sh tools/*.sh", "scripts/*.sh #tools/*.sh"), ["tools/x.sh"]) == ["tools/x.sh"]


def test_operator_ends_the_arguments():
    for sc in ("scripts/*.sh || echo tools/*.sh", "scripts/*.sh; echo tools/*.sh", "scripts/*.sh > tools/*.sh"):
        assert cl.uncovered(SYN("scripts/*.sh tools/*.sh", sc), ["tools/x.sh"]) == ["tools/x.sh"], sc
    assert cl.uncovered(SYN("scripts/*.sh", "scripts/*.sh;"), ["scripts/a.sh"]) == []


def test_options_are_not_globs():
    assert cl.uncovered(SYN("scripts/*.sh", "-x scripts/*.sh"), ["scripts/a.sh"]) == []
