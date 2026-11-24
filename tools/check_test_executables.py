"""Report every place under tests/ that makes an executable in the test process (Issues 374, 379).

A file a test writes in its own process and then runs can fail to exec with ETXTBSY ("Text file
busy"): a child another test thread forks inherits the write descriptor until its own exec.
tests/common/exe.rs makes executables in a `cp` child; this tool reads every `*.rs` file under
`tests/` except that helper and reports, at its line:

- `from_mode(`, `.mode(` or `set_mode(` whose argument is not an octal literal without an execute
  bit (a mode with any bit of 0o111, or an expression this tool cannot judge);
- `fs::copy(` (writes the copy in this process and keeps the source's mode);
- `Command::new("chmod")`.

A `// not an executable: <reason>` comment on the reported line or on the line directly above it
exempts the line (a directory's mode, a copy of a file that is never run).
Run: python3 tools/check_test_executables.py [root]. The tests job runs it through
tests/test_check_test_executables.py."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HELPER = "tests/common/exe.rs"
ADVICE = ("make the executable with common::exe (tests/common/exe.rs), or mark a line that makes"
          " none with `// not an executable: <why>`")
MARKER = re.compile(r"//.*\bnot an executable:\s*\S")
# The argument of a mode call, up to its closing parenthesis (no nesting is needed for a literal).
MODE_CALL = re.compile(r"(?:\bfrom_mode|\.mode|\bset_mode)\s*\(\s*([^()]*?)\s*,?\s*\)", re.S)
LITERAL = re.compile(r"0o([0-7_]+)")
COPY = re.compile(r"\bfs::copy\s*\(")
CHMOD = re.compile(r'\bCommand::new\(\s*"chmod"\s*\)')


def _sites(text):
    """(offset, what) of each site in text, in order."""
    sites = []
    for m in MODE_CALL.finditer(text):
        arg = m.group(1)
        if arg == "":
            continue  # `.mode()`: the getter
        lit = LITERAL.fullmatch(arg)
        if lit is None:
            sites.append((m.start(), f"sets a mode this tool cannot judge ({' '.join(arg.split())})"))
        elif int(lit.group(1).replace("_", ""), 8) & 0o111:
            sites.append((m.start(), f"sets an executable mode ({arg})"))
    sites += [(m.start(), "copies a file in this process (fs::copy keeps the source's mode)")
              for m in COPY.finditer(text)]
    sites += [(m.start(), "runs chmod") for m in CHMOD.finditer(text)]
    return sorted(sites)


def check_text(name, text):
    """One finding `<name>:<line>: <what>; <advice>` per site of text not exempted by the marker."""
    lines = text.splitlines()
    findings = []
    for offset, what in _sites(text):
        n = text.count("\n", 0, offset) + 1
        here = lines[n - 1] if n <= len(lines) else ""
        above = lines[n - 2] if n >= 2 else ""
        if MARKER.search(here) or MARKER.search(above):
            continue
        findings.append(f"{name}:{n}: {what}; {ADVICE}")
    return findings


def check(root):
    """The findings of every `*.rs` file under `<root>/tests/` except the helper."""
    root = Path(root)
    findings = []
    for p in sorted((root / "tests").rglob("*.rs")):
        rel = p.relative_to(root).as_posix()
        if rel == HELPER or not p.is_file():
            continue
        findings += check_text(rel, p.read_text(encoding="utf-8"))
    return findings


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else ROOT
    findings = check(root)
    print("\n".join(findings) or "test executables ok")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
