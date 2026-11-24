#!/bin/sh
# Renders docs/design/screen-draft.html with wrapcheck.js injected into a copy (the drawing itself
# keeps no script) and prints what the check found. Run from the repository root:
#   sh docs/design/screen-checks/wrapcheck.sh [page.html]
# The browser is $CHROME if set, else a headless Chromium found on this machine. Exit 1 when a
# line holds a single word or a row is cut, 2 when no browser or no result.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
page=${1:-docs/design/screen-draft.html}
browser=${CHROME:-}
if [ -z "$browser" ]; then
  for b in "$HOME"/.cache/ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell \
           "$(command -v chromium 2>/dev/null)" "$(command -v chromium-browser 2>/dev/null)" \
           "$(command -v google-chrome 2>/dev/null)"; do
    if [ -n "$b" ] && [ -x "$b" ]; then browser=$b; fi
  done
fi
if [ -z "$browser" ]; then echo "wrapcheck: no headless Chromium; set CHROME" >&2; exit 2; fi
work=$(mktemp -d "${TMPDIR:-/tmp}/wrapcheck.XXXXXX")
trap 'rm -rf "$work"' EXIT
python3 - "$page" "$here/wrapcheck.js" "$work/check.html" <<'PY'
import sys, pathlib
page, js, out = (pathlib.Path(a) for a in sys.argv[1:])
html = page.read_text(encoding="utf-8")
out.write_text(html.replace("</body>", "<script>" + js.read_text(encoding="utf-8") + "</script></body>"), encoding="utf-8")
PY
"$browser" --no-sandbox --window-size=1480,1000 --virtual-time-budget=3000 --dump-dom "file://$work/check.html" 2>/dev/null \
  | python3 -c '
import sys, re, html
m = re.search(r"<pre id=\"wrapcheck\">(.*?)</pre>", sys.stdin.read(), re.S)
if not m:
    print("wrapcheck: no result"); sys.exit(2)
text = html.unescape(m.group(1)); print(text)
sys.exit(0 if "no line holds a single word" in text and "no row is cut" in text else 1)'
