"""Check docs/design/incidents.md: every row names a Test file that exists (G2).

A missing file is allowed only if listed in docs/design/incidents-pending.txt
(one path per line, '#' comments); that list must be empty by M7-1.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def check(table, pending, root):
    pend = {l.strip() for l in Path(pending).read_text(encoding="utf-8").splitlines()
            if l.strip() and not l.lstrip().startswith("#")}
    errors, cols = [], None
    for line in Path(table).read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if cols is None:
            cols = cells  # header row
            if "Test" not in cols:
                return ["no 'Test' column"]
            continue
        if set("".join(cells)) <= set("-: "):
            continue  # separator row
        row = cells[0]
        files = re.findall(r"`([^`]+)`", cells[cols.index("Test")])
        if not files:
            errors.append(f"{row}: empty Test cell")
        for f in files:
            if not (Path(root) / f).is_file() and f not in pend:
                errors.append(f"{row}: {f} does not exist and is not pending")
    return errors


if __name__ == "__main__":
    errs = check(ROOT / "docs/design/incidents.md", ROOT / "docs/design/incidents-pending.txt", ROOT)
    print("\n".join(errs) or "incident table ok")
    sys.exit(1 if errs else 0)
