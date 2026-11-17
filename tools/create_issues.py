#!/usr/bin/env python3
"""Create the GitHub Issues from issues/*.md, in order, and fill in the numbers.

    python3 tools/create_issues.py --repo OWNER/NAME            # dry run: prints what it would do
    python3 tools/create_issues.py --repo OWNER/NAME --apply    # creates them with `gh`

Each file's first line (`# M0-1 Title`) is the Issue title. A last line
`Depends on M0-1, M0-2` becomes `Depends on #12, #13`: claude-org-ja's triage reads
references only from the same line as the keyword. Issues are created in the order of
issues/index.md, so every dependency already has a number. The id-to-number map is
written to issues/numbers.json; a second run skips ids already in it.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ID = re.compile(r"\bM\d-\d+\b")


def order(root: Path) -> list[str]:
    return re.findall(r"^\| \[(M\d-\d+)\]", (root / "issues" / "index.md").read_text(encoding="utf-8"), flags=re.M)


def render(text: str, numbers: dict[str, int]) -> tuple[str, str]:
    lines = text.rstrip("\n").split("\n")
    title = lines[0].removeprefix("# ").strip()
    body = lines[1:]
    if body and body[-1].startswith("Depends on "):
        missing = [i for i in ID.findall(body[-1]) if i not in numbers]
        if missing:
            raise SystemExit(f"{title}: dependency without a number yet: {missing}")
        body[-1] = ID.sub(lambda m: f"#{numbers[m.group(0)]}", body[-1])
    return title, "\n".join(body).strip() + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--apply", action="store_true")
    a = ap.parse_args()
    root = Path(__file__).resolve().parent.parent
    store = root / "issues" / "numbers.json"
    numbers: dict[str, int] = json.loads(store.read_text()) if store.exists() else {}
    fake = max(numbers.values(), default=0)
    for i in order(root):
        if i in numbers:
            continue
        title, body = render((root / "issues" / f"{i}.md").read_text(encoding="utf-8"), numbers)
        if a.apply:
            out = subprocess.run(["gh", "issue", "create", "--repo", a.repo, "--title", title, "--body", body,
                                  "--label", i.split("-")[0]], check=True, capture_output=True, text=True).stdout
            numbers[i] = int(out.strip().rsplit("/", 1)[-1])
            store.write_text(json.dumps(numbers, indent=1) + "\n")
        else:
            fake += 1
            numbers[i] = fake
            dep = body.rstrip().split("\n")[-1]
            print(f"#{fake:<3} {title}" + (f"   [{dep}]" if dep.startswith("Depends on") else ""))
    if not a.apply:
        print("\ndry run: nothing created. Labels M0..M7 must exist before --apply.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
