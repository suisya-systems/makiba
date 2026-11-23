"""How tools/shell_lint_coverage.py reads globs as bash does (Issue 302, review of the fix)."""
import importlib.util
from pathlib import Path

import pytest

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


def test_only_the_severity_option_is_read():
    # An option's value is not a script: --rcfile tools/x.sh does not lint tools/x.sh.
    for sc in ("--rcfile tools/x.sh scripts/*.sh tools/*.sh", "-x scripts/*.sh tools/*.sh",
               "scripts/*.sh -e SC1000 tools/*.sh", "--severity scripts/*.sh tools/*.sh"):
        with pytest.raises(ValueError):
            cl.uncovered(SYN("scripts/*.sh tools/*.sh", sc), ["tools/x.sh"])
    assert cl.uncovered(SYN("scripts/*.sh", "--severity=warning scripts/*.sh"), ["scripts/a.sh"]) == []


def test_glob_syntax_bash_reads_differently_raises():
    for g in ("scripts/[^x]*.sh", "scripts/[!x]*.sh", "scripts/{a,b}.sh", '"scripts/a b.sh"', "scripts/\\*.sh",
              "$DIR/*.sh", "~/x.sh"):
        with pytest.raises(ValueError):
            cl.uncovered(SYN(g, "scripts/*.sh"), ["scripts/x.sh"])
        with pytest.raises(ValueError):
            cl.uncovered(SYN("scripts/*.sh", g), ["scripts/x.sh"])


def loop_body(body):
    return (
        "jobs:\n"
        "  lint:\n"
        "    steps:\n"
        "      - name: Shell scripts parse, and shellcheck finds no error\n"
        "        run: |\n"
        f"          for f in scripts/*.sh tools/*.sh; do {body} || exit 1; done\n"
        "          shellcheck --severity=error scripts/*.sh tools/*.sh\n"
    )


def test_loop_must_parse_its_own_variable():
    # Unquoted $f splits "scripts/x.sh y.sh", so bash -n parses scripts/x.sh instead: refused.
    for body in ("bash -n scripts/install-base.sh", 'bash -n "$g"', "bash -n $ff", "echo bash -n",
                 "bash -n $f", "bash -n ${f}", 'bash -n "$ff"'):
        with pytest.raises(ValueError):
            cl.uncovered(loop_body(body), ["tools/x.sh"])
    for body in ('bash -n "$f"', 'bash -n "${f}"'):
        assert cl.uncovered(loop_body(body), ["tools/x.sh"]) == [], body
