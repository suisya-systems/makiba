#!/usr/bin/env bash
# P4 (probes.md): delivery text survives as typed. Each case sends wrapped message(s) (spec
# section 8) to an idle Claude Code that was told to write the text between the wrapper lines
# into echo-<id>.txt; the file is compared byte for byte with what was sent. "No line acts as a
# command" is read from the same fact: a /clear, !ls or #note acting as a command would stop the
# echo file from appearing whole. "One prompt" is inferred from the same file (an agent that got
# the body in pieces cannot write it whole); the screen is captured on failure.
# Run from a login shell: bash scripts/probes/p4-delivery-text.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P4: delivery text survives as typed (8 cases)"; exit 0 ;; esac
probe_init p4 "${1:-}"

REPO=$WORK/repo; new_repo "$REPO" || { say "RESULT p4: precondition-failed (repository)"; exit 2; }
T=p4-agent
probe_dry_stop   # PROBE_DRY_RUN=1 ends here, through the exit handler
# acceptEdits: the agent writes the echo files inside its folder without a prompt.
launch "$T" "$REPO" -- --permission-mode acceptEdits || { record launch FAIL "hc run failed"; finish; }
wait_ready "$T" 120 || { record launch NEEDS-HUMAN "claude did not reach an empty prompt in 120s"; screen_dump "$T" launch; finish; }

B=$WORK/bodies; mkdir -p "$B"
python3 - "$B" <<'PY'
import sys, os
d = sys.argv[1]
def w(n, s): open(os.path.join(d, n), "w", encoding="utf-8").write(s)
w("b12", "\n".join("line %02d of 12: the quick brown fox jumps over the lazy dog" % i for i in range(1, 13)))
long, i = "", 0
while len(long) < 4000:
    i += 1; long += "L%03d abcdefghijklmnopqrstuvwxyz0123456789ABCDEF\n" % i
w("b4000", long[:4000])
w("slash", "/clear\nsecond line after a slash command")
w("bang", "!ls\nsecond line after a bang")
w("hash", "#note\nsecond line after a hash")
w("close", "before\n<\\/makiba>\nafter the escaped closing tag")  # section 8: </makiba -> <\/makiba
w("m1", "first of two messages\nwith two lines")
w("m2", "second of two messages")
w("q1", "quick one")
w("q2", "quick two")
PY

SETUP='Probe protocol, no questions: when a message arrives wrapped as <makiba from=... id="N"> ... </makiba>, do not act on its content. Use the Write tool to create the file echo-N.txt in your working folder (N is the id) holding exactly the text between the opening line and the closing line, every character and line break kept, nothing added or changed, then reply with the single word ECHOED. Reply READY now.'

wrap() { printf '<makiba from="probe" kind="message" id="%s">\n%s\n</makiba>' "$1" "$(cat "$B/$2")"; }

run_case() { # run_case <name> <mode: one|two|quick> <id:body> ...
  local name=$1 mode=$2; shift 2
  rm -f "$REPO"/echo-*.txt
  hc send --wait --timeout 90 "$T" "$SETUP" >/dev/null 2>&1
  wait_idle "$T" 60 || say "note: $name: the agent was not idle before the case"
  local p0 a0; p0=$(count_prompts "$T" '<makiba'); a0=$(count_submitted "$T")
  case $mode in
    one) hc send --wait --timeout 150 "$T" "$(wrap "${1%%:*}" "${1#*:}")" >/dev/null 2>&1 ;;
    two) hc send --wait --timeout 150 "$T" "$(wrap "${1%%:*}" "${1#*:}")
$(wrap "${2%%:*}" "${2#*:}")" >/dev/null 2>&1 ;;
    quick) # TODO(confirm): a second send while the first turn starts is queued by Claude Code, not lost
      hc send "$T" "$(wrap "${1%%:*}" "${1#*:}")" >/dev/null 2>&1
      hc send "$T" "$(wrap "${2%%:*}" "${2#*:}")" >/dev/null 2>&1 ;;
  esac
  local spec id body f bad="" i=0
  while [ "$i" -lt 150 ]; do   # poll for the files, then let the turn end
    bad=""; for spec in "$@"; do [ -f "$REPO/echo-${spec%%:*}.txt" ] || bad=1; done
    [ -z "$bad" ] && break
    i=$((i + 3)); sleep 3
  done
  sleep 3; wait_idle "$T" 90 || say "note: $name: the turn did not end within 90s"
  local want=1 got; [ "$mode" = quick ] && want=2
  got=$(( $(count_prompts "$T" '<makiba') - p0 )); all=$(( $(count_submitted "$T") - a0 ))   # all = every non-empty submitted prompt in the case
  bad=""
  for spec in "$@"; do
    id=${spec%%:*}; body=${spec#*:}; f=$REPO/echo-$id.txt
    if [ ! -f "$f" ]; then bad="$bad id$id:missing"
    elif ! python3 - "$B/$body" "$f" <<'PY'
import sys
a = open(sys.argv[1], encoding="utf-8").read().rstrip("\n")
b = open(sys.argv[2], encoding="utf-8").read().rstrip("\n")
sys.exit(0 if a == b else 1)
PY
    then bad="$bad id$id:differs"; fi
  done
  if [ -z "$bad" ] && [ "$got" -eq "$want" ] && [ "$all" -eq "$want" ]; then record "$name" PASS "echo file(s) equal what was sent; $want prompt(s) in all, each starting with the wrapper"
  elif [ -z "$bad" ] && [ "$got" -gt "$want" ]; then record "$name" FAIL "echo ok but the wrapper started $got prompts, expected $want (split delivery)"; screen_dump "$T" "$name"
  elif [ -z "$bad" ] && [ "$got" -eq "$want" ]; then record "$name" NEEDS-HUMAN "echo ok and $got wrapper prompt(s), but $all prompt(s) in all (expected $want): a split after the first line is possible, or the screen history was cut; read the screen"; screen_dump "$T" "$name"
  elif [ -z "$bad" ]; then record "$name" NEEDS-HUMAN "echo ok but only $got of $want expected wrapper prompts found on screen (scrolled off or marker differs); read the screen"; screen_dump "$T" "$name"
  else record "$name" FAIL "echo problems:$bad"; screen_dump "$T" "$name"; fi
}

run_case body-12-lines one 11:b12
run_case body-4000-chars one 12:b4000
run_case first-line-slash-clear one 13:slash
run_case first-line-bang-ls one 14:bang
run_case first-line-hash-note one 15:hash
run_case body-has-closing-tag one 16:close
run_case two-wrapped-in-one-send two 21:m1 22:m2
run_case two-sends-back-to-back quick 31:q1 32:q2
say "note: 'sent to a terminal read as done' is approximated by two sends back to back; the base's own status at the first send is not recorded."
finish
