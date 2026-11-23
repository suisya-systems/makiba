"""Lint every tracked shell script, the list taken from git itself (Issue 354).

Each file of `git ls-files -z -- '*.sh'` (any depth, any name) is parsed with `bash -n` and,
when it parses, checked with `shellcheck --severity=error`. No glob list has to follow the
tree. A shellcheck that cannot be started is an error, never a skip.
Run: python3 tools/shell_lint.py [root]. The tests job runs it through tests/test_shell_lint.py."""
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def tracked_scripts(root):
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", "*.sh"],
        check=True, capture_output=True,
    ).stdout
    return [p for p in out.decode("utf-8", "surrogateescape").split("\0") if p]


def _run(argv, root):
    r = subprocess.run(argv, cwd=root, capture_output=True, text=True, errors="replace")
    return r.returncode, (r.stdout + r.stderr).strip()


def lint(root, shellcheck="shellcheck"):
    """One failure line per tracked script that does not parse or that shellcheck finds an error in."""
    if shutil.which(shellcheck) is None:
        raise RuntimeError(f"cannot run shellcheck: {shellcheck!r} is not an executable on PATH")
    fails = []
    for path in tracked_scripts(root):
        # ./ and -- keep a name that starts with '-' a file operand.
        operand = "./" + path
        if not (Path(root) / path).is_file():
            fails.append(f"{path}: tracked but missing from the worktree")
            continue
        rc, out = _run(["bash", "-n", "--", operand], root)
        if rc != 0:
            fails.append(f"{path}: bash -n failed: {out}")
            continue
        try:
            rc, out = _run([shellcheck, "--severity=error", "--", operand], root)
        except OSError as e:
            raise RuntimeError(f"cannot run shellcheck {shellcheck!r}: {e}") from e
        if rc != 0:
            fails.append(f"{path}: shellcheck --severity=error failed: {out}")
    return fails


def main(argv=None, shellcheck="shellcheck"):
    argv = sys.argv[1:] if argv is None else argv
    root = Path(argv[0]) if argv else ROOT
    try:
        fails = lint(root, shellcheck=shellcheck)
    except RuntimeError as e:
        print(f"shell_lint: {e}", file=sys.stderr)
        return 2
    for f in fails:
        print(f)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
