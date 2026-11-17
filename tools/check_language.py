"""Fail when a file under makiba/, roles/, skills/, docs/, tests/ or knowledge/ holds
Hiragana, Katakana or CJK Unified Ideographs, except under the paths in
tools/language-allowlist.txt (M0-7, G7)."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIRS = ("makiba", "roles", "skills", "docs", "tests", "knowledge")
CJK = re.compile("[぀-ゟ゠-ヿ一-鿿]")


def check(root, allow):
    root = Path(root)
    allowed = tuple(
        l.strip() for l in Path(allow).read_text(encoding="utf-8").splitlines()
        if l.strip() and not l.lstrip().startswith("#")
    )
    errors = []
    for d in DIRS:
        for p in sorted((root / d).rglob("*")):
            rel = p.relative_to(root).as_posix()
            if not p.is_file() or rel.startswith(allowed) or "__pycache__" in p.parts:
                continue
            try:
                text = p.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue  # binary file
            if m := CJK.search(text):
                line = text.count("\n", 0, m.start()) + 1
                errors.append(f"{rel}:{line}: non-English character U+{ord(m[0]):04X}")
    return errors


if __name__ == "__main__":
    errs = check(ROOT, ROOT / "tools" / "language-allowlist.txt")
    print("\n".join(errs) or "language ok")
    sys.exit(bool(errs))
