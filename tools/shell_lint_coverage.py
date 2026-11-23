"""Fail when a tracked shell script is outside the lint job's shell step (Issue 302).

The lint job in .github/workflows/ci.yml names its scripts by globs, twice: in the
`bash -n` loop and on the `shellcheck --severity=error` line. Each tracked *.sh
(git ls-files, any depth) must be matched by a glob of both, or it goes unchecked.
Run: python3 tools/shell_lint_coverage.py [root]. The tests job runs it through
tests/test_shell_lint_coverage.py."""
import subprocess
import sys
from fnmatch import fnmatchcase
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CI = Path(".github") / "workflows" / "ci.yml"
STEP = "- name: Shell scripts parse"
LOOP = "for f in "
SHELLCHECK = "shellcheck --severity=error "


def _indent(line):
    return len(line) - len(line.lstrip(" "))


def _run_block(ci_text):
    lines = ci_text.splitlines()
    start = next((i for i, l in enumerate(lines) if l.strip().startswith(STEP)), None)
    if start is None:
        raise ValueError(f"no step starting {STEP!r} in the workflow")
    run = next((i for i in range(start + 1, len(lines)) if lines[i].strip() == "run: |"), None)
    if run is None or any(lines[i].strip().startswith("- ") for i in range(start + 1, run)):
        raise ValueError("the shell step has no `run: |` block")
    depth = _indent(lines[run])
    block = []
    for l in lines[run + 1:]:
        if l.strip() and _indent(l) <= depth:
            break
        block.append(l.strip())
    return block


def lint_globs(ci_text):
    """The globs of the `bash -n` loop and of the shellcheck line of the lint job's shell step."""
    block = _run_block(ci_text)
    loop = next((l for l in block if l.startswith(LOOP) and "; do" in l and "bash -n" in l), None)
    sc = next((l for l in block if l.startswith(SHELLCHECK)), None)
    if loop is None or sc is None:
        raise ValueError("the shell step has no `for f in ...; do bash -n` loop or no shellcheck line")
    return loop[len(LOOP):loop.index("; do")].split(), sc[len(SHELLCHECK):].split()


def _matches(glob, path):
    # A bash glob does not cross '/': match segment by segment.
    g, p = glob.split("/"), path.split("/")
    return len(g) == len(p) and all(fnmatchcase(s, pat) for s, pat in zip(p, g))


def uncovered(ci_text, files):
    loop, sc = lint_globs(ci_text)
    return sorted(
        f for f in files
        if not any(_matches(g, f) for g in loop) or not any(_matches(g, f) for g in sc)
    )


def tracked_scripts(root):
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", "*.sh"],
        check=True, capture_output=True,
    ).stdout
    return [p for p in out.decode("utf-8", "surrogateescape").split("\0") if p]


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    root = Path(argv[0]) if argv else ROOT
    ci_text = (root / CI).read_text(encoding="utf-8")
    missing = uncovered(ci_text, tracked_scripts(root))
    for f in missing:
        print(f"{f}: tracked shell script not covered by the lint job's shell step ({CI.as_posix()})")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
