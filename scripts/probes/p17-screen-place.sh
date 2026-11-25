#!/usr/bin/env bash
# Issue 404 (probes.md P17): where the person's screen can run. The measurement M1 to M4 of
# docs/design/screen.md section 5, run by the owner on the machine where herdr-app runs, against the real
# organization (`makiba up` done, makibad running). The app's side is done by hand: this script prints each action
# as a numbered step, waits for the owner's Enter, asks what the app showed, and checks on the base's side
# whatever can be checked there.
#
# What it touches: it starts no core, no makibad and no agent. It reads the organization's status with
# `makiba status --json`, reads `commands` and `pending` of the real state.db read-only (sqlite3 mode=ro, as
# `makiba status` reads it), and serves a stand-in page on 127.0.0.1 under a random path for the run's length.
# It never makes a pending question and never answers one itself: the case m2-answer is run only when a real
# question waits and the owner chooses to answer it from the app.
#
# Cases (each line "CASE <name>: PASS|FAIL|NEEDS-HUMAN|NOT-RUN  <detail>"):
#  m1-command  The app runs `makiba status --json` on the base's machine and shows its output, refreshed
#              without the owner's hand (the owner's answer; the script prints the heartbeat to compare).
#  m1-page     M1 by the other way, a request to 127.0.0.1 of the base's host: the app shows the stand-in page
#              anywhere (a tab, a panel) and it refreshes: the owner's answer and at least two reads of the page's
#              status.json counted by the server during a 15-second window.
#  m3-page     The same page shown beside the secretary's terminal: m1-page's conditions and the owner's answer.
#  m2-act      The app runs `makiba config get conversation_language` (a person-only verb that changes nothing):
#              PASS when commands gains a row by actor person, verb config, not rejected, and the owner saw the
#              result in the app. A row by another actor (unknown: no key reached the door) is FAIL.
#  m2-answer   Only when an escalated question waits and the owner answers it from the app: PASS when that entry
#              is answered with answered_by person. Otherwise NOT-RUN.
# M4 (what an addition to the app is made of and how it is installed) is a paragraph the owner writes into
# probes.md by hand; the script records only the owner's one-line summary.
#
# Run from the person's login shell, never from an agent:
#   MAKIBA_BIN=$(command -v makiba) bash scripts/probes/p17-screen-place.sh <result-dir>
# Env: MAKIBA_BIN (default makiba), P17_PORT (default 0: the system chooses), HC_BIN (only for its version).
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P17 (Issue 404): where the screen can run, M1 to M4 of screen.md section 5, by hand in herdr-app with a real organization (env MAKIBA_BIN, P17_PORT)"; exit 0 ;; esac
MAKIBA_BIN=${MAKIBA_BIN:-makiba}
type -P "$MAKIBA_BIN" >/dev/null 2>&1 || { echo "RESULT p17: precondition-failed (MAKIBA_BIN=$MAKIBA_BIN not found)"; exit 2; }
MAKIBA_BIN=$(type -P "$MAKIBA_BIN")
case "$MAKIBA_BIN" in /*) ;; *) MAKIBA_BIN=$PWD/$MAKIBA_BIN ;; esac

# probe_init moves XDG_CONFIG_HOME and XDG_STATE_HOME to scratch folders and sets HC_SESSION; this probe reads the
# person's real organization, so `makiba` is run with the environment the owner started the script with.
REAL_UNSET=(); REAL_SET=()
for v in XDG_CONFIG_HOME XDG_STATE_HOME XDG_RUNTIME_DIR HC_SESSION MAKIBA_STATE_DIR MAKIBA_PERSON_KEY MAKIBA_DOOR; do
  if [ -n "${!v+x}" ]; then REAL_SET+=("$v=${!v}"); else REAL_UNSET+=(-u "$v"); fi
done
REAL_DB=${MAKIBA_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/makiba}/state.db
mk() { env ${REAL_UNSET[@]+"${REAL_UNSET[@]}"} ${REAL_SET[@]+"${REAL_SET[@]}"} "$MAKIBA_BIN" "$@"; }

probe_init p17 "${1:-}"
say "MAKIBA_BIN: $MAKIBA_BIN"
say "state.db (read-only): $REAL_DB"
PAGE_PID=""
extra_cleanup() { [ -n "$PAGE_PID" ] && kill "$PAGE_PID" 2>/dev/null; return 0; }
probe_dry_stop   # PROBE_DRY_RUN=1 ends here, through the exit handler

[ -r /dev/tty ] || { say "RESULT p17: precondition-failed (no terminal to read the owner's answers from)"; exit 2; }
say "makiba --version: $(mk --version 2>&1 | head -n 1)"

status_json() { mk status --json 2>/dev/null; }
jfield() { # jfield <python expression over d>: from status --json on stdin
  python3 -c 'import json,sys
try: d = json.load(sys.stdin)
except Exception: print(""); sys.exit(0)
print(eval(sys.argv[1]))' "$1"
}
db() { # db <sql> [params...]: one read-only query of the real state.db, rows tab-separated
  python3 - "$REAL_DB" "$@" <<'PY'
import pathlib, sqlite3, sys
# as_uri escapes ?, # and %, so mode=ro is never cut off by the path
c = sqlite3.connect(pathlib.Path(sys.argv[1]).resolve().as_uri() + "?mode=ro", uri=True, timeout=5)
# A parameter of digits is bound as an integer: SQLite never finds a text equal to an integer id.
args = [int(a) if a.isdigit() else a for a in sys.argv[3:]]
for row in c.execute(sys.argv[2], args):
    print("\t".join("" if v is None else str(v) for v in row))
PY
}
ask() { # ask <question>: y, n or s from the owner; sets ANSWER
  local a
  while :; do
    printf '%s [y/n/s=skip] ' "$1" > /dev/tty; IFS= read -r a < /dev/tty || a=s
    case "$a" in y|n|s) ANSWER=$a; return 0 ;; esac
  done
}
line() { # line <prompt>: one line from the owner; sets LINE
  printf '%s ' "$1" > /dev/tty; IFS= read -r LINE < /dev/tty || LINE=""
}
step() { # step <n> <text>: a numbered action for the owner, then Enter
  say "STEP $1: $2"
  printf '   ... press Enter when done ' > /dev/tty; IFS= read -r _ < /dev/tty || true
}

# ---- preconditions: a running organization, the state.db readable
S=$(status_json)
[ "$(printf '%s' "$S" | jfield 'd.get("makibad",{}).get("running")')" = True ] \
  || { say "RESULT p17: precondition-failed (makiba status --json does not show makibad running; run makiba up first)"; exit 2; }
db 'SELECT 1' >/dev/null 2>&1 || { say "RESULT p17: precondition-failed (cannot read $REAL_DB read-only)"; exit 2; }
say "organization: makibad running, heartbeat $(printf '%s' "$S" | jfield 'd["makibad"].get("heartbeat_at")')"
line "1. The app's version (as the app shows it in its About or settings):"; say "herdr-app version: $LINE"
line "2. The version of hc on the app's side (for example hc.exe --version on Windows):"; say "hc on the app's side: $LINE"
line "3. Where the app runs and where the base runs (for example: app on Windows, base in WSL):"; say "machines: $LINE"

# ---- M1 by a command the app runs
say "-- M1 by a command"
step 4 "Make the app run \`makiba status --json\` on the base's machine and show its output, without typing it into a terminal yourself (an addition of the app, a button, a panel). If the app has no way to, go on."
ask "5. Does the app show that JSON, and does it change by itself (its heartbeat_at moves; now $(status_json | jfield 'd["makibad"].get("heartbeat_at")'))?"
case $ANSWER in
  y) record m1-command PASS "the owner saw the status JSON in the app, refreshed without their hand" ;;
  n) record m1-command FAIL "the app did not show the status JSON refreshed by itself" ;;
  s) say "CASE m1-command: NOT-RUN  skipped by the owner" ;;
esac

# ---- M3 (and M1 by a request to 127.0.0.1): the stand-in page
say "-- M3 the stand-in page on 127.0.0.1"
PAGE=$WORK/page; mkdir -p "$PAGE"
cat > "$PAGE/serve.py" <<'PY'
# The stand-in of the screen's page: 127.0.0.1 only, one random path, no request to any other host.
import http.server, json, secrets, subprocess, sys, os
state, argv = sys.argv[1], sys.argv[2:]
token = secrets.token_hex(16)
HTML = """<!doctype html><meta charset="utf-8"><title>makiba P17</title>
<body style="font:16px system-ui;margin:24px"><h1>makiba P17 stand-in</h1>
<p>Reads: <b id="n">0</b> &middot; last read: <b id="t">-</b></p>
<p>makibad running: <b id="r">-</b> &middot; heartbeat: <b id="h">-</b> &middot; questions waiting: <b id="q">-</b></p>
<script>
let n = 0;
async function tick() {
  try {
    const d = await (await fetch("status.json", {cache: "no-store"})).json();
    n += 1;
    document.getElementById("n").textContent = n;
    document.getElementById("t").textContent = new Date().toLocaleTimeString();
    document.getElementById("r").textContent = String(d.makibad && d.makibad.running);
    document.getElementById("h").textContent = String(d.makibad && d.makibad.heartbeat_at);
    document.getElementById("q").textContent = String((d.pending || []).filter(p => p.status === "escalated").length);
  } catch (e) { document.getElementById("t").textContent = "error: " + e; }
}
tick(); setInterval(tick, 5000);
</script>"""
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, ctype, body):
        self.send_response(code); self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store"); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        if self.path == "/%s/" % token:
            return self.send(200, "text/html; charset=utf-8", HTML.encode())
        if self.path == "/%s/status.json" % token:
            out = subprocess.run(argv, capture_output=True, timeout=20).stdout
            with open(os.path.join(state, "reads"), "a") as f: f.write("x\n")
            return self.send(200, "application/json", out)
        self.send(404, "text/plain", b"not found\n")
port = int(os.environ.get("P17_PORT", "0"))
s = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
with open(os.path.join(state, "ready.tmp"), "w") as f: f.write("http://127.0.0.1:%d/%s/" % (s.server_address[1], token))
os.replace(os.path.join(state, "ready.tmp"), os.path.join(state, "ready"))
s.serve_forever()
PY
: > "$PAGE/reads"
env ${REAL_UNSET[@]+"${REAL_UNSET[@]}"} ${REAL_SET[@]+"${REAL_SET[@]}"} python3 "$PAGE/serve.py" "$PAGE" "$MAKIBA_BIN" status --json &
PAGE_PID=$!
i=0; while [ ! -s "$PAGE/ready" ] && [ "$i" -lt 20 ]; do sleep 0.5; i=$((i + 1)); done
[ -s "$PAGE/ready" ] || { say "RESULT p17: precondition-failed (the stand-in page did not start)"; exit 2; }
URL=$(cat "$PAGE/ready")
say "stand-in page served (127.0.0.1 only, for this run; the URL's path is random and is not recorded)"
printf '   The page: %s\n' "$URL" > /dev/tty
step 6 "Open that page in the app (as the app frames a web page, if it can), beside the secretary's terminal if the app allows. Leave it open for at least 15 seconds."
: > "$PAGE/reads"
step 7 "Keep it open for another 15 seconds, then press Enter."
READS=$(wc -l < "$PAGE/reads" | tr -d ' ')
ask "8. Is the page shown inside the app (anywhere: a tab, a panel), with its read count going up?"
SHOWN=$ANSWER
if [ "$SHOWN" = s ]; then say "CASE m1-page: NOT-RUN  skipped by the owner ($READS reads during step 7)"
elif [ "$SHOWN" = y ] && [ "$READS" -ge 2 ]; then record m1-page PASS "shown in the app, refreshed; $READS reads of status.json during step 7 (a request to 127.0.0.1 of the base's host)"
elif [ "$SHOWN" = y ]; then record m1-page FAIL "the owner saw it, but the server counted $READS read(s) during step 7 (the page was not the one served here, or did not refresh)"
else record m1-page FAIL "not shown in the app ($READS reads during step 7)"; fi
if [ "$SHOWN" = s ]; then say "CASE m3-page: NOT-RUN  follows m1-page"
elif [ "$SHOWN" = y ] && [ "$READS" -ge 2 ]; then
  ask "8b. Is it beside the secretary's terminal, both in view at once?"
  case $ANSWER in
    y) record m3-page PASS "shown beside the secretary's terminal, refreshed" ;;
    n) record m3-page FAIL "shown and refreshed, but not beside the secretary's terminal" ;;
    s) say "CASE m3-page: NOT-RUN  skipped by the owner" ;;
  esac
else record m3-page FAIL "m1-page did not pass"; fi
kill "$PAGE_PID" 2>/dev/null; PAGE_PID=""

# ---- M2 acting as the person
say "-- M2 acting as the person"
MARK=$(db 'SELECT COALESCE(MAX(id),0) FROM commands')
step 9 "Make the app run \`makiba config get conversation_language\` as you, outside any agent's terminal, and show its answer. It changes nothing. Do not type it into a terminal yourself; if the app can only type it into a terminal it shows, let it do that and say so at step 11."
ask "10. Did the app show the answer (a language, or that none is set)?"
SEEN=$ANSWER
line "11. How did the app run it: extension (an addition of the app ran it), terminal (the app typed it into a shell terminal), or none?"
WAY=${LINE:-none}
# Only the request of step 9: verb config, action get, key conversation_language (src/cli.rs sends these args).
ROWS=$(db "SELECT id, actor, verb, status FROM commands WHERE id > ? AND verb = 'config'
  AND json_extract(args_json, '\$.action') = 'get' AND json_extract(args_json, '\$.key') = 'conversation_language' ORDER BY id" "$MARK")
say "commands since $MARK of config get conversation_language: $(printf '%s' "$ROWS" | tr '\t\n' ' ;')"
if [ "$SEEN" = s ]; then say "CASE m2-act: NOT-RUN  skipped by the owner (way: $WAY)"
elif printf '%s\n' "$ROWS" | awk -F'\t' '$2 != "person" && $2 != ""' | grep -q .; then
  record m2-act FAIL "a config request reached the door as another actor (no person key went with it); way: $WAY"
elif printf '%s\n' "$ROWS" | awk -F'\t' '$2 == "person" && $4 != "rejected"' | grep -q . && [ "$SEEN" = y ]; then
  record m2-act PASS "commands records actor person, verb config; the answer shown in the app; way: $WAY"
else
  record m2-act FAIL "no accepted config request by person after the mark, or the answer not shown; way: $WAY"
fi

WAITING=$(status_json | jfield '" ".join(str(p["id"]) for p in d.get("pending",[]) if p.get("status") == "escalated")')
if [ -z "$WAITING" ]; then
  say "CASE m2-answer: NOT-RUN  no question waits for the person (the probe never makes one: an answer is delivered to a worker)"
else
  line "12. Questions waiting for you: $WAITING. To answer one of them from the app as you would anyway, type its id here and do not answer it yet (step 13 asks for that); or press Enter to skip:"
  ID=$LINE
  case "$ID" in ''|*[!0-9]*) ID="" ;; esac
  case " $WAITING " in *" $ID "*) ;; *) [ -n "$ID" ] && say "entry $ID is not one of the questions waiting: skipped"; ID="" ;; esac
  if [ -z "$ID" ]; then say "CASE m2-answer: NOT-RUN  skipped by the owner"
  else
    MARK2=$(db 'SELECT COALESCE(MAX(id),0) FROM commands')
    step 13 "Now answer entry $ID from the app (\`makiba answer $ID yes|no\`), as at step 9."
    ENTRY=$(db 'SELECT status, answered_by FROM pending WHERE id = ?' "$ID")
    # The answer of this run: a new request by person, verb answer, for this entry, not rejected.
    NEWANS=$(db "SELECT COUNT(*) FROM commands WHERE id > ? AND actor = 'person' AND verb = 'answer'
      AND json_extract(args_json, '\$.id') = ? AND status != 'rejected'" "$MARK2" "$ID")
    if [ "$ENTRY" = "answered	person" ] && [ "${NEWANS:-0}" -ge 1 ]; then
      record m2-answer PASS "entry $ID answered by person through a new answer request; way: $WAY"
    else
      record m2-answer FAIL "entry $ID is now: $(printf '%s' "$ENTRY" | tr '\t' ' '); new answer requests by person for it: ${NEWANS:-0}"
    fi
  fi
fi

# ---- M4
line "14. M4 in one line (what an addition to the app is made of, how it is installed, and what can start it: only a click in the app, or also a request from another local program). Write the paragraph into probes.md P17 by hand:"
say "M4 (owner's summary): $LINE"
finish
