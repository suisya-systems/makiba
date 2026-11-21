# shellcheck shell=bash
# Shared helpers for the makiba host probes (P1, P3, P4, P5). Sourced, not run.
# Every probe: own HC_SESSION, scratch XDG folders, strips session-bound variables,
# prints versions first, writes a result file, stops only its own core on exit.
# Every hc call here follows paddock/scripts/probes (measured with hc 0.1.18); a line whose
# behaviour was not measured there carries "TODO(confirm)".

# Variables removed before anything is launched: docs/design/spec.md section 6.4.
UNSET_VARS="CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_SESSION_ID CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN CLAUDECODE CLAUDE_CODE_SESSION_ATTENDED CLAUDE_PID CLAUDE_TMPDIR GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN"
# Added by the base for its own terminals; unset here so the probe never points at another core.
BASE_VARS="HC_SOCKET HC_SCOPE HC_TERMINAL_ID HC_TERMINAL_SOCKET"

TERMS=""        # terminals this probe created (killed on exit)
FAILS=0
DIALOGS=0       # trust dialogs answered

# Every hc call is bounded: a hung hc must show as a failed call (status 124), never as a silent stop of the probe.
hc() { if [ -n "${HC_TIMEOUT_BIN:-}" ]; then "$HC_TIMEOUT_BIN" "${HC_CALL_TIMEOUT:-60}" "$HC_BIN" "$@"; else "$HC_BIN" "$@"; fi; }

probe_usage() { # probe_usage <script> <what>
  printf 'usage: %s <result-dir>\n  %s\n  env: HC_BIN (default: hc), see scripts/probes/README.md\n' "$1" "$2"
}

ver_ge() { # ver_ge <a> <b>: a >= b, dotted numbers
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$2" ]
}

probe_init() { # probe_init <name> <result-dir> [min-hc-version]
  PROBE=$1; OUTDIR=$2; MINVER=${3:-}
  [ -n "$OUTDIR" ] || { echo "RESULT $PROBE: precondition-failed (give a result directory)"; exit 2; }
  HC_BIN=${HC_BIN:-hc}
  for c in "$HC_BIN" claude python3 git mktemp sort; do
    type -P "$c" >/dev/null 2>&1 && continue
    probe_dry && { [ "$c" = "$HC_BIN" ] || [ "$c" = claude ]; } && continue   # a dry run calls neither
    echo "RESULT $PROBE: precondition-failed ($c not on PATH)"; exit 2
  done
  HC_TIMEOUT_BIN=$(type -P timeout 2>/dev/null || true)   # coreutils; without it hc calls are unbounded
  HC_BIN=$(type -P "$HC_BIN" || type -P false)   # not command -v: that would find the hc function below; false: a dry run without hc
  mkdir -p "$OUTDIR" || exit 2
  RESULT="$OUTDIR/$PROBE-result.txt"
  HCVER=$(hc --version 2>&1 | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n 1)
  CLVER=$(claude --version 2>&1 | head -n 1)
  HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  COMMIT=$(git -C "$HERE" rev-parse --short HEAD 2>/dev/null || echo unknown)
  [ -z "$(git -C "$HERE" status --porcelain -- "$HERE" 2>/dev/null)" ] || COMMIT="$COMMIT+dirty"
  : > "$RESULT"
  say "probe: $PROBE"
  say "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  say "script commit: $COMMIT"
  say "HC_BIN: $HC_BIN"
  say "hc --version: $(hc --version 2>&1 | head -n 1)"
  say "claude --version: $CLVER"
  if [ -n "$MINVER" ] && ! probe_dry && { [ -z "$HCVER" ] || ! ver_ge "$HCVER" "$MINVER"; }; then
    say "RESULT $PROBE: REFUSED (hc $HCVER is below $MINVER; this probe measures a $MINVER behaviour. Point HC_BIN at a newer hc.)"
    exit 3
  fi
  # Strip session-bound variables, so a run started from another context does not leak them.
  for v in $UNSET_VARS $BASE_VARS; do unset "$v"; done
  # Own core: nothing of the person's default core is touched.
  SESSION=makiba-probe-$$
  export HC_SESSION=$SESSION
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/makiba-$PROBE-XXXXXX") || exit 2
  export XDG_CONFIG_HOME=$WORK/config XDG_STATE_HOME=$WORK/state XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp}
  mkdir -p "$XDG_CONFIG_HOME/herdr-core" "$XDG_STATE_HOME/herdr-core" || exit 2
  printf 'notes = false\n' > "$XDG_CONFIG_HOME/herdr-core/config.toml"   # hc types nothing on its own (hc --help)
  say "session: $SESSION  scratch: $WORK"
  trap probe_cleanup EXIT
  trap 'exit 130' INT TERM
}

# PROBE_DRY_RUN=1: the script runs up to its probe_dry_stop line, before the base, makibad or an agent is
# started, and exits there through its exit handler (CI runs each probe this way once, Issue 105).
probe_dry() { [ "${PROBE_DRY_RUN:-}" = 1 ]; }
probe_dry_stop() {
  probe_dry || return 0
  say "RESULT $PROBE: dry run (PROBE_DRY_RUN=1), stopped before the base, makibad or an agent is started"
  exit 0
}

say() { printf '%s\n' "$*" | tee -a "$RESULT"; }

probe_cleanup() {
  trap - EXIT
  declare -F extra_cleanup >/dev/null && extra_cleanup
  if [ "${HC_SESSION:-}" = "${SESSION:-x}" ]; then   # only ever our own core
    for t in $TERMS; do hc kill "$t" >/dev/null 2>&1; hc rm "$t" >/dev/null 2>&1; done
    hc server stop >/dev/null 2>&1
  fi
  [ -n "${WORK:-}" ] && rm -rf "$WORK"
}

record() { # record <case> <PASS|FAIL|NEEDS-HUMAN> <detail>
  say "CASE $1: $2  $3"
  [ "$2" = FAIL ] && FAILS=$((FAILS + 1))
  return 0
}

screen_dump() { # screen_dump <terminal> <label>: captured screen text into the result file
  { echo "---- screen of $1 ($2)"; hc read --lines 80 "$1" 2>&1; echo "---- end"; } >> "$RESULT"
}

finish() {
  say "SUMMARY $PROBE: $FAILS failing case(s)${PARTIAL:+; partial run, $PARTIAL, the other cases NOT-RUN}${FLAGGED_STEPS:+; flagged step(s) (not pass, not fail):$FLAGGED_STEPS}; result file $RESULT"
  [ "$FAILS" -eq 0 ]; exit $?
}

new_repo() { # new_repo <dir>: a fresh git repository no person has trusted
  mkdir -p "$1" && git init -q "$1" && git -C "$1" -c user.name=probe -c user.email=probe@example.invalid commit -q --allow-empty -m probe
}

launch() { # launch <name> <cwd> [VAR=value ...] -- <claude args...>
  local name=$1 cwd=$2; shift 2
  local extra=()
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do extra+=("$1"); shift; done
  shift
  local u=(); for v in $UNSET_VARS; do u+=(-u "$v"); done
  TERMS="$TERMS $name"
  hc run --bg --no-parent --name "$name" --cwd "$cwd" -- env "${u[@]}" ${extra[@]+"${extra[@]}"} claude "$@" >/dev/null 2>&1
}

state_of() { hc get --json "$1" 2>/dev/null | tr -d '\n' | sed -n 's/.*"status" *: *{[^}]*"state" *: *"\([a-z_]*\)".*/\1/p'; }

has_dialog() { case "$(hc read "$1" 2>/dev/null)" in *"Quick safety check"*|*"trust this folder"*) return 0 ;; esac; return 1; }

answer_dialog() { # the folder-trust policy: move the cursor onto a "Yes" line, read again, then one key
  local t=$1 i line
  for i in 1 2 3; do
    has_dialog "$t" || return 0
    line=$(hc read "$t" 2>/dev/null | grep '❯' | head -n 1)
    case "$line" in
      *Yes*) hc keys "$t" enter >/dev/null 2>&1; DIALOGS=$((DIALOGS + 1)); sleep 3; return 0 ;;
      *) hc keys "$t" down >/dev/null 2>&1; sleep 1 ;;
    esac
  done
  return 1
}

is_ready() { # empty prompt, no dialog
  has_dialog "$1" && return 1
  case "$(state_of "$1")" in idle|done) return 0 ;; esac
  hc read "$1" 2>/dev/null | grep -Eq 'for shortcuts|\? for'
}

wait_ready() { # wait_ready <terminal> <seconds>: answers the trust dialog on the way
  local i=0
  while [ "$i" -lt "$2" ]; do
    has_dialog "$1" && answer_dialog "$1"
    is_ready "$1" && return 0
    i=$((i + 2)); sleep 2
  done
  return 1
}

wait_idle() { # wait_idle <terminal> <seconds>
  local i=0
  while [ "$i" -lt "$2" ]; do
    case "$(state_of "$1")" in idle|done) return 0 ;; esac   # done = a finished turn (OSC 7501)
    i=$((i + 2)); sleep 2
  done
  return 1
}

count_prompts() { # count_prompts <terminal> <regex>: prompt lines (composer marker first) in the history
  hc read --lines 3000 "$1" 2>/dev/null | grep -cE "^[[:space:]]*[❯>] *$2"   # TODO(confirm): marker is "❯"
}

count_submitted() { # count_submitted <terminal>: every prompt line in the history except the live composer (the last one)
  # Measured 2026-11-18 (p4-result.txt): counting only non-empty prompt lines gave 0 in two cases because the
  # live composer sometimes shows a grey suggestion, so it was counted before and not after. The last marker line is always the composer.
  local n; n=$(hc read --lines 3000 "$1" 2>/dev/null | grep -cE '^[[:space:]]*[❯>]')
  echo $(( n > 0 ? n - 1 : 0 ))
}

# ---- channel probes (p8, p9) ----
term_info() { # term_info <terminal>: "<state> <turn>" from hc get --json (status.state, and turn searched anywhere, TODO(confirm));
  # "none none" when hc failed, the JSON is invalid or a field is missing or of the wrong kind: such a sample is never a state
  hc get --json "$1" 2>/dev/null | python3 -c '
import json, sys
def find(o, k):
    if isinstance(o, dict):
        if k in o and not isinstance(o[k], (dict, list)): return o[k]
        for v in o.values():
            r = find(v, k)
            if r is not None: return r
    elif isinstance(o, list):
        for v in o:
            r = find(v, k)
            if r is not None: return r
try: d = json.load(sys.stdin)
except Exception: d = {}
st = d.get("status") if isinstance(d, dict) else None
state = st.get("state") if isinstance(st, dict) else None
turn = find(d, "turn")
ok = isinstance(state, str) and state.isalpha() and isinstance(turn, int) and not isinstance(turn, bool)
print("%s %s" % (state, turn) if ok else "none none")'
}

pass_dialogs() { # pass_dialogs <terminal> <seconds>: the startup-dialog policy; every new dialog screen goes to the result file
  DLG_KEYS=0; DLG_ACCEPTED=""; local max=$2 t=$1 i=0 prev="$WORK/dlg.prev" cur="$WORK/dlg.cur" out last="" n=0
  : > "$prev"
  while [ "$i" -lt "$max" ]; do
    hc read "$t" > "$cur" 2>/dev/null
    out=$(python3 "$HERE/dialog-step.py" --prev "$prev" < "$cur")
    case "$out" in
      done) DLG_KEYS=$n; say "dialogs: finished after $n key(s)"; return 0 ;;
      key*) { echo "---- dialog screen seen by $t ($out)"; cat "$cur"; echo "----"; } >> "$RESULT"
            set -- $out; declare -F dialog_hook >/dev/null && { dialog_hook before "$t" "$2" "$3" || true; }   # optional, record-only: a probe may define dialog_hook <before|after> <terminal> <key> <dialog>
            hc keys "$t" "$2" >/dev/null 2>&1; declare -F dialog_hook >/dev/null && { dialog_hook after "$t" "$2" "$3" || true; }
            [ "$2" = enter ] && DLG_ACCEPTED="$DLG_ACCEPTED $3"; n=$((n + 1)); DLG_KEYS=$n; : > "$prev"; sleep 1.5; i=$((i + 2)); continue ;;
      unknown*) say "dialogs: $out"; { echo "---- unknown dialog screen of $t"; cat "$cur"; echo "----"; } >> "$RESULT"; return 2 ;;
    esac
    cp -f "$cur" "$prev"; i=$((i + 2)); sleep 2
  done
  say "dialogs: no composer after $max s (last: $out)"; return 1
}

standin_start() { # standin_start: starts channel-standin.py on a free loopback port; sets PORT TOKEN CRED ADMIN STATE MCPCFG
  STATE=$WORK/standin; mkdir -p "$STATE"
  TOKEN=DECOYTOKEN$(python3 -c 'import secrets;print(secrets.token_hex(8))'); CRED=DECOYCRED$$; ADMIN=DECOYADMIN$$
  # secrets go through a private file, not argv or environ, so P9's scan sees only what the client side exposes
  ( umask 077; printf '{"token":"%s","cred":"%s","admin":"%s"}' "$TOKEN" "$CRED" "$ADMIN" > "$WORK/standin-secrets.json" )
  python3 "$HERE/channel-standin.py" serve --port 0 --state "$STATE" --secrets "$WORK/standin-secrets.json" &
  STANDIN_PID=$!
  local i=0; while [ ! -s "$STATE/ready" ] && [ "$i" -lt 20 ]; do sleep 0.5; i=$((i + 1)); done
  [ -s "$STATE/ready" ] || return 1
  PORT=$(cat "$STATE/ready")   # the server bound port 0 itself: no window between choosing a port and binding it
  MCPCFG=$WORK/mcp.json
  # Shape of claude-org-runtime tokens.py (http server with a bearer header) and channel_server_config (stdio server).
  python3 - "$MCPCFG" "$PORT" "$TOKEN" "$HERE" "$(type -P python3)" "$CRED" <<'PY'
import json, sys
cfg, port, token, here, py, cred = sys.argv[1:7]
json.dump({"mcpServers": {
  "makiba": {"type": "http", "url": "http://127.0.0.1:%s/mcp" % port, "headers": {"Authorization": "Bearer " + token}},
  "makiba-channel": {"command": py, "args": [here + "/channel-standin.py", "channel"],
                     "env": {"STANDIN_URL": "http://127.0.0.1:%s" % port, "STANDIN_CRED": cred, "STANDIN_NAME": "makiba-channel"}}}},
  open(cfg, "w"), indent=2)
PY
}

admin() { # admin '<json>'
  python3 - "http://127.0.0.1:$PORT/admin" "$ADMIN" "$1" <<'PY'
import sys, urllib.request
r = urllib.request.Request(sys.argv[1], data=sys.argv[3].encode(), method="POST", headers={"Authorization": "Bearer " + sys.argv[2]})
print(urllib.request.build_opener(urllib.request.ProxyHandler({})).open(r, timeout=5).read().decode())   # loopback only: no proxy from the environment
PY
}
enqueue() { admin "$(python3 -c 'import json,sys;print(json.dumps({"op":"enqueue","content":sys.argv[1]}))' "$1")"; }
