#!/usr/bin/env python3
"""Create the GitHub Issues from issues/*.md, in order, and fill in the numbers.

    python3 tools/create_issues.py --repo OWNER/NAME            # dry run: prints what it would do
    python3 tools/create_issues.py --repo OWNER/NAME --apply    # creates them with `gh`
    python3 tools/create_issues.py --repo OWNER/NAME --update   # dry run: which existing Issues changed
    python3 tools/create_issues.py --repo OWNER/NAME --update --apply   # edits them with `gh issue edit`

Each file's first line (`# M0-1 Title`) is the Issue title. A last line
`Depends on M0-1, M0-2` becomes `Depends on #12, #13`: claude-org-ja's triage reads
references only from the same line as the keyword. Issues are created in the order of
issues/index.md, so every dependency already has a number. The id-to-number map is
written to issues/numbers.json; a second run skips ids already in it.

--update works on the ids already in issues/numbers.json: it reads each Issue with
`gh issue view`, compares its title and body with the regenerated ones, prints those that
differ and, with --apply, writes them with `gh issue edit --body-file` (and `--title` when
the title changed). It never reopens an Issue: it lists the ids whose Issue is closed and
whose body carries the heading build_issues.REPLACES (reworked after a merge), each with
the command that reopens it.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from build_issues import REPLACES

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


def gh(*args: str) -> str:
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def update(root: Path, repo: str, numbers: dict[str, int], apply: bool) -> int:
    reopen = []
    for i in order(root):
        if i not in numbers:
            continue
        n = numbers[i]
        title, body = render((root / "issues" / f"{i}.md").read_text(encoding="utf-8"), numbers)
        cur = json.loads(gh("issue", "view", str(n), "--repo", repo, "--json", "title,body,state"))
        changed = [k for k, new in (("title", title), ("body", body)) if cur[k].strip() != new.strip()]
        if changed:
            print(f"#{n:<3} {i}: {' and '.join(changed)} changed ({cur['state'].lower()})")
            if apply:
                with tempfile.NamedTemporaryFile("w", suffix=".md", encoding="utf-8", delete=False) as f:
                    f.write(body)
                extra = ["--title", title] if "title" in changed else []
                gh("issue", "edit", str(n), "--repo", repo, "--body-file", f.name, *extra)
                Path(f.name).unlink()
        if cur["state"] == "CLOSED" and REPLACES in body:
            reopen.append((i, n))
    if not apply:
        print("\ndry run: nothing edited.", file=sys.stderr)
    for i, n in reopen:
        print(f"reopen {i}: gh issue reopen {n} --repo {repo}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--update", action="store_true", help="edit the Issues already in issues/numbers.json")
    a = ap.parse_args()
    root = Path(__file__).resolve().parent.parent
    store = root / "issues" / "numbers.json"
    numbers: dict[str, int] = json.loads(store.read_text()) if store.exists() else {}
    if a.update:
        return update(root, a.repo, numbers, a.apply)
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
