#!/usr/bin/env python3
"""One step of the startup-dialog policy (python3 standard library only).

Reads a screen on stdin, optionally the previous screen from --prev FILE, prints one line:
  done                  the composer is visible, no dialog remains
  wait <reason>         nothing to press yet
  key <down|up|enter> <dialog>   the one key to press next, and which dialog it is for
  unknown <reason>      a selection dialog that is not on the policy list: press nothing
Policy list (option text matched whole, spaceless, case-insensitive; never by position):
  folder-trust  accept "Yes, I trust this folder" / "Yes, proceed"; reject "No, exit"
  dev-channel   accept "I am using this for local development"; reject "Exit"
The cursor must be on the accept row (then enter) or on the reject row (then move toward accept),
and the screen must equal the previous one (settled) before any key. Option texts were read in
claude-org-runtime dispatcher/spawn_prompt.py (2026-11-18); the dialog wording itself is recorded
by the probes, not assumed.
"""
import re
import sys
import unicodedata

CUR = ("❯", ">")


def norm(s):
    return " ".join(unicodedata.normalize("NFKC", s).split()).lower()


def label(n):
    n = n.strip("│| ")
    for c in CUR:
        if n.startswith(c):
            n = n[len(c):].lstrip()
            break
    return re.sub(r"^\d+[.)]\s*", "", n).replace(" ", "")


def main():
    prev = None
    if len(sys.argv) > 2 and sys.argv[1] == "--prev":
        try:
            prev = [norm(x) for x in open(sys.argv[2], encoding="utf-8").read().splitlines()]
        except OSError:
            prev = None
    lines = [norm(x) for x in sys.stdin.read().splitlines()]
    kinds = {
        "folder-trust": (("yes,itrustthisfolder", "yes,proceed"), ("no,exit",)),
        "dev-channel": (("iamusingthisforlocaldevelopment",), ("exit",)),
    }
    found = {}
    for k, (acc, rej) in kinds.items():
        a = [i for i, n in enumerate(lines) if any(label(n).startswith(x) for x in acc)]
        r = [i for i, n in enumerate(lines) if label(n) in rej]
        if a:
            found[k] = (a, r)
    if len(found) > 1:
        print("wait both dialogs visible")
        return
    if found:
        k = next(iter(found))
        a, r = found[k]
        top = min(a + r)
        cursors = [i for i, n in enumerate(lines) if i >= top and n.strip("│| ").startswith(CUR)]
        if len(a) > 1 or len(r) > 1 or len(cursors) > 1:
            print("unknown %s: duplicate option rows or more than one cursor" % k)
            return
        cur = next((i for i in cursors if i == a[0] or i in r), None)
        if cur is None:
            print("wait %s: no cursor on an option row yet" % k)
            return
        def trim(rows):
            while rows and not rows[-1]:
                rows = rows[:-1]
            return rows

        if prev is None or trim(lines) != trim(prev):
            print("wait %s: screen not settled" % k)
            return
        print("key %s %s" % ("enter" if cur == a[0] else ("down" if a[0] > cur else "up"), k))
        return
    if any(f in "".join(n.split()) for n in lines for f in ("entertoconfirm", "entertoselect", "↑/↓tonavigate")):
        print("unknown selection dialog not on the policy list")
        return
    for i, n in enumerate(lines):
        if n.startswith(CUR[0]) and any(set(x.strip("╭╮╰╯")) <= set("─━-") and len(x) >= 8
                                        for x in lines[max(0, i - 2):i] + lines[i + 1:i + 3]):
            print("done")
            return
    print("wait no dialog or composer yet")


main()
