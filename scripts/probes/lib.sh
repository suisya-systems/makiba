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

hc() { "$HC_BIN" "$@"; }

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
    type -P "$c" >/dev/null 2>&1 || { echo "RESULT $PROBE: precondition-failed ($c not on PATH)"; exit 2; }
  done
  HC_BIN=$(type -P "$HC_BIN")   # not command -v: that would find the hc function below
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
  if [ -n "$MINVER" ] && { [ -z "$HCVER" ] || ! ver_ge "$HCVER" "$MINVER"; }; then
    say "RESULT $PROBE: REFUSED (hc $HCVER is below $MINVER; this probe measures a $MINVER behaviour. Point HC_BIN at a newer hc.)"
    exit 3
  fi
  # Strip session-bound variables, so a run started from another context does not leak them.
  for v in $UNSET_VARS $BASE_VARS; do unset "$v"; done
  # Own core: nothing of the person's default core is touched.
  SESSION=makiba-probe-$$
  export HC_SESSION=$SESSION
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/makiba-$PROBE.XXXXXX") || exit 2
  export XDG_CONFIG_HOME=$WORK/config XDG_STATE_HOME=$WORK/state XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp}
  mkdir -p "$XDG_CONFIG_HOME/herdr-core" "$XDG_STATE_HOME/herdr-core" || exit 2
  printf 'notes = false\n' > "$XDG_CONFIG_HOME/herdr-core/config.toml"   # hc types nothing on its own (hc --help)
  say "session: $SESSION  scratch: $WORK"
  trap probe_cleanup EXIT
  trap 'exit 130' INT TERM
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
  say "SUMMARY $PROBE: $FAILS failing case(s); result file $RESULT"
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
