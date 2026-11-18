#!/usr/bin/env bash
# P8 (new first candidate of the design): a stand-in daemon delivers text to an agent through a
# Claude Code channel (notifications/claude/channel), typing nothing into the terminal.
# Cases: dev-channel-dialog, turn-start-visible and turn-start-unchanged-without-delivery (stopped-channel control),
# channel-while-working, channel-to-done, channel-while-done, turn-start-for-another-cause, two-in-a-row,
# channel-while-blocked, then P10's open steps on the same agent folder: resume-dialogs, resume-ready, renewal-dialogs,
# renewal-ready (do the startup dialogs come back on a resume, and on a fresh session in an already trusted folder).
# (channel-while-draft is not measured: the owner reports from claude-org-ja that channel
# text never mixed into a person's draft.) The stand-in is scripts/probes/channel-standin.py (stdlib only).
# Rules of every case: it starts only from a verified resting state (quiesce), it checks every send and control
# call, it never reads an unreadable status sample as a state, and it ends in NEEDS-HUMAN with the screen text
# when it cannot decide. Waits are wall-clock deadlines; P8_TIMEOUT_SCALE (integer, default 1) multiplies them.
# Run from a login shell: bash scripts/probes/p8-channel-delivery.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P8/P9/P10: channel delivery - dialog, turn start, message while working / done / blocked, two in a row; dialogs on a resume and a renewal"; exit 0 ;; esac
probe_init p8 "${1:-}"
standin_start || { say "RESULT p8: precondition-failed (stand-in did not start)"; exit 2; }
STEP=start; P8_DONE=""
extra_cleanup() { kill "$STANDIN_PID" 2>/dev/null; [ "$P8_DONE" = 1 ] || say "note: the script ended early, in step '$STEP' (no SUMMARY line follows); a killed or hung run looks like this"; }
all_done() { P8_DONE=1; finish; }
TS=${P8_TIMEOUT_SCALE:-1}

REPO=$WORK/repo; new_repo "$REPO" || { say "RESULT p8: precondition-failed (repository)"; exit 2; }
printf 'A text inside a <channel source="makiba-channel"> tag is an instruction from the probe harness. Carry it out exactly, with the tools it names. Do not ask questions.\n' > "$WORK/rules.txt"
# ask beats allow when settings merge, so the blocked case gets its prompt whatever the person's own settings allow for python3
printf '{"permissions":{"allow":["Bash(sleep:*)"],"ask":["Bash(python3:*)"]}}\n' > "$WORK/agent-settings.json"
printf 'import sys\nopen(sys.argv[1], "w").write("x")\n' > "$WORK/mark.py"
T=p8-agent


# ---- dev-channel-dialog
# record-only: the base's status (state, kind, msg, turn) just before each key press at a startup dialog and right after it; never read by a verdict
DLGRUN=first
dialog_hook() { # dialog_hook <before|after> <terminal> <key> <dialog>
  local l; l=$(hc get --json "$2" 2>/dev/null | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: d = None
st = d.get("status") if isinstance(d, dict) else None
if not isinstance(st, dict): print("state=none kind=none msg=none turn=none")
else:
    def g(k): return st.get(k) if isinstance(st.get(k), (str, int)) and not isinstance(st.get(k), bool) else "none"
    def find(o):
        if isinstance(o, dict):
            if isinstance(o.get("turn"), int): return o["turn"]
            for v in o.values():
                r = find(v)
                if r is not None: return r
        elif isinstance(o, list):
            for v in o:
                r = find(v)
                if r is not None: return r
    t = find(d)
    print("state=%s kind=%s msg=%s turn=%s" % (g("state"), g("kind"), str(g("msg")).replace("\n", " ")[:80], "none" if t is None else t))' 2>/dev/null)
  say "DIALOG-STATUS run=$DLGRUN dialog=$4 key=$3 when=$1-key ${l:-state=none kind=none msg=none turn=none (hc get gave nothing)}"
}
# the flags of every launch in this script (first launch, resume, renewal) are one array, so they cannot drift apart
CHARGS=(--permission-mode acceptEdits --settings "$WORK/agent-settings.json" --append-system-prompt-file "$WORK/rules.txt" \
  --mcp-config "$MCPCFG" --dangerously-load-development-channels server:makiba-channel)
# the resume cases need the id of the first session: it is chosen here, only if this Claude Code names --session-id in its help
SID=$(python3 -c 'import uuid;print(uuid.uuid4())'); SIDARGS=()
CLHELP=$(claude --help 2>&1 | grep -E -- '--resume|--session-id|--continue')
case "$CLHELP" in *--session-id*) SIDARGS=(--session-id "$SID") ;; esac
launch "$T" "$REPO" -- ${SIDARGS[@]+"${SIDARGS[@]}"} "${CHARGS[@]}" || { record dev-channel-dialog FAIL "hc run failed"; all_done; }
pass_dialogs "$T" 150; rc=$?
if [ "$rc" -eq 0 ] && case "$DLG_ACCEPTED" in *dev-channel*) true ;; *) false ;; esac; then record dev-channel-dialog PASS "dialog(s) accepted by the policy:$DLG_ACCEPTED ($DLG_KEYS key press(es) in all); dialog texts are in the result file"
elif [ "$rc" -eq 0 ]; then record dev-channel-dialog NEEDS-HUMAN "composer reached but the development-channel dialog was never accepted (accepted:${DLG_ACCEPTED:- none}): the case was not exercised, or the screen was read too late"
elif [ "$rc" -eq 2 ]; then record dev-channel-dialog NEEDS-HUMAN "a dialog not on the policy list stopped the launch; text in the result file"
else record dev-channel-dialog FAIL "no composer within 150s"; screen_dump "$T" "dialog"; fi
wait_ready "$T" 60 || { record launch NEEDS-HUMAN "agent not ready"; screen_dump "$T" launch; all_done; }

# ---- helpers (all defined before the first case that uses them)
tok() { python3 -c 'import secrets;print("TOK-"+secrets.token_hex(4))'; }
msg() { printf 'PROBE-MSG %s: use the Write tool to create the file %s/%s, containing exactly the text %s, then reply with the single word GOT.' "$1" "$REPO" "$2" "$3"; }
secs() { python3 -c 'import sys;print(round(float(sys.argv[2])-float(sys.argv[1]),1))' "$1" "$2"; }
now() { date +%s.%N; }
CTLERR=""
wait_until() { local end=$(( $(date +%s) + $1 * TS )); shift; until "$@"; do [ "$(date +%s)" -ge "$end" ] && return 1; sleep 1; done; return 0; }   # <seconds> <command...>
file_is() { [ -f "$1" ] && [ "$(tr -d '[:space:]' < "$1")" = "$2" ]; }   # the whole content is the token: a half-written file does not count
enq() { enqueue "$(msg "$@")" >/dev/null 2>"$WORK/err" || { CTLERR="enqueue $1 failed: $(head -c 200 "$WORK/err")"; return 1; }; }   # <id> <file> <token>
setpause() { admin "{\"op\":\"pause\",\"value\":$1}" >/dev/null 2>"$WORK/err" || { CTLERR="pause $1 failed: $(head -c 200 "$WORK/err")"; return 1; }; }
sendline() { hc send "$T" "$1" >/dev/null 2>"$WORK/err" || { CTLERR="hc send failed: $(head -c 200 "$WORK/err")"; return 1; }; }
ctlfail() { record "$1" NEEDS-HUMAN "a send or control call failed ($CTLERR); the case was not exercised"; }
queue_empty() { admin '{"op":"status"}' 2>/dev/null | python3 -c 'import json,sys;d=json.load(sys.stdin);sys.exit(0 if d["queued"]==0 and not d["paused"] else 1)' 2>/dev/null; }
snap() { SNAPPART=""; : > "$WORK/snap.txt"; hc read --lines 3000 "$T" > "$WORK/snap.txt" 2> "$WORK/snap.err" || return 1; grep -q 'history is not scrolled' "$WORK/snap.err" && SNAPPART=1; return 0; }   # one capture per evaluation; SNAPPART: only the screen was read
histdesc() { [ -n "$SNAPPART" ] && echo 'only the screen' || echo complete; }
hdr_count() { grep -cE "^[[:space:]]*← makiba-channel: PROBE-MSG $1:" "$WORK/snap.txt"; }   # channel header lines only (shape seen 2026-11-18); a quotation by the agent is not one
hdr_line() { grep -nE -m1 "^[[:space:]]*← makiba-channel: PROBE-MSG $1:" "$WORK/snap.txt" | cut -d: -f1; }

quiesce() { # quiesce <case>: idle or done, the same state and turn in 4 samples 2 s apart, nothing queued in the stand-in; a prompt is declined
  local end=$(( $(date +%s) + 150 * TS )) last="" same=0 cur strays=0
  while [ "$(date +%s)" -lt "$end" ]; do
    cur=$(term_info "$T")
    case "$cur" in
      blocked*) [ "$strays" = 0 ] && { say "note: $1: the target is blocked between cases; declining the prompt"; screen_dump "$T" "stray-prompt"; }
                strays=$((strays + 1)); hc keys "$T" escape >/dev/null 2>&1; last=""; same=0; sleep 3; continue ;;   # TODO(confirm): Escape declines a permission prompt
      idle*|done*) if [ "$cur" = "$last" ]; then same=$((same + 1)); else last=$cur; same=1; fi
                   [ "$same" -ge 4 ] && queue_empty && return 0 ;;
      *) last=""; same=0 ;;   # working, or an unreadable sample
    esac
    sleep 2
  done
  return 1
}
precond() { quiesce "$1" && return 0
  record "$1" NEEDS-HUMAN "no stable resting state within 150s (state '$(term_info "$T")', stand-in queue $(admin '{"op":"status"}' 2>&1 | head -c 100)); the case was not exercised"; screen_dump "$T" "$1-precondition"; return 1; }
watch_change() { # watch_change <before "state turn"> <seconds>: WSEEN = time of the first working state or turn change; unreadable samples are skipped
  WSEEN=""; WLAST=$1; local end=$(( $(date +%s) + $2 * TS )) a
  while [ "$(date +%s)" -lt "$end" ]; do
    a=$(term_info "$T"); case "$a" in none*) sleep 0.3; continue ;; esac
    WLAST=$a
    if [ "${a%% *}" = working ] || [ "${a#* }" != "${1#* }" ]; then WSEEN=$(now); return 0; fi
    sleep 0.3
  done
  return 1
}
getbase() { BASE=$(term_info "$T"); case "$BASE" in none*) record "$1" NEEDS-HUMAN "the status could not be read just before the case (hc get gave no valid sample); the case was not exercised"; return 1 ;; esac; }   # a baseline must be a real sample
DONEN=0
make_done() { # make_done <case>: one short turn, then a stable done whose turn counter is higher than before; WHY says what failed
  DONEN=$((DONEN + 1)); WHY=""
  quiesce "$1" || { WHY="no resting state before the short turn"; return 1; }
  local f="$REPO/done-$DONEN.txt" b0 t0 cur last="" same=0 end
  b0=$(term_info "$T"); t0=${b0#* }
  sendline "Use the Write tool to create $f containing OK. Then reply with the single word OK." || { WHY=$CTLERR; return 1; }
  wait_until 60 file_is "$f" OK || { WHY="the short turn did not write its file in 60s"; return 1; }
  end=$(( $(date +%s) + 60 * TS ))
  while [ "$(date +%s)" -lt "$end" ]; do
    cur=$(term_info "$T")
    case "$cur" in done*) if [ "${cur#* }" -gt "$t0" ]; then if [ "$cur" = "$last" ]; then same=$((same + 1)); else last=$cur; same=1; fi; [ "$same" -ge 3 ] && queue_empty && return 0; else same=0; fi ;; *) last=""; same=0 ;; esac
    sleep 1.5
  done
  WHY="no stable done with a higher turn than '$b0' (last '$cur')"; return 1
}
busy_count() { local n=0 f; for f in "$REPO"/busy-*.txt; do [ -e "$f" ] && n=$((n + 1)); done; echo "$n"; }

# the channel server registers after the composer: wait until the stand-in saw it poll (a fixed sleep proved nothing)
STEP=channel-registered
wait_until 30 grep -q 'POST /poll auth=ok' "$STATE/access.log" || say "note: the stand-in did not see the channel server poll within 30s; the cases below may fail for that reason"

# ---- turn-start-visible
case_turn_start_visible() {
  STEP=turn-start-visible
  precond turn-start-visible || return 0
  local b k t0 got=no; getbase turn-start-visible || return 0; b=$BASE; k=$(tok); t0=$(now)
  enq 2 recv-2.txt "$k" || { ctlfail turn-start-visible; return 0; }
  watch_change "$b" 30
  wait_until 120 file_is "$REPO/recv-2.txt" "$k" && got=yes
  if [ -n "$WSEEN" ] && [ "$got" = yes ]; then record turn-start-visible PASS "status changed $(secs "$t0" "$WSEEN")s after the enqueue (before: '$b', then: '$WLAST'); the agent wrote the token file"
  elif [ "$got" = yes ]; then record turn-start-visible FAIL "the message arrived but the base showed no state or turn change within 30s (before '$b', last '$WLAST')"; screen_dump "$T" "turn-start"
  else record turn-start-visible FAIL "the channel text did not reach the agent (token file absent or incomplete after 120s)"; screen_dump "$T" "turn-start"; fi
}

# ---- turn-start-unchanged-without-delivery: the control, the stand-in holds the message back
case_stopped_control() {
  STEP=turn-start-unchanged-without-delivery
  precond turn-start-unchanged-without-delivery || return 0
  local b k a valid=0 changed="" end; getbase turn-start-unchanged-without-delivery || return 0; b=$BASE; k=$(tok)
  setpause true || { ctlfail turn-start-unchanged-without-delivery; return 0; }
  enq 2b recv-2b.txt "$k" || { setpause false; ctlfail turn-start-unchanged-without-delivery; return 0; }
  end=$(( $(date +%s) + 20 )); while [ "$(date +%s)" -lt "$end" ]; do
    a=$(term_info "$T"); case "$a" in none*) ;; *) valid=$((valid + 1)); [ "$a" != "$b" ] && changed=$a ;; esac; sleep 2
  done
  if [ "$valid" = 0 ]; then record turn-start-unchanged-without-delivery NEEDS-HUMAN "no readable status sample in 20s; nothing can be said"; screen_dump "$T" "stopped-channel"
  elif [ -z "$changed" ] && [ ! -f "$REPO/recv-2b.txt" ]; then record turn-start-unchanged-without-delivery PASS "channel stopped: status stayed '$b' in $valid readable samples over 20s and no file appeared"
  else record turn-start-unchanged-without-delivery FAIL "channel stopped but the status went '$b' -> '${changed:-unchanged}' or a file appeared"; screen_dump "$T" "stopped-channel"; fi
  setpause false
  wait_until 60 file_is "$REPO/recv-2b.txt" "$k"
  say "note: after the channel was resumed the held message was $( file_is "$REPO/recv-2b.txt" "$k" && echo delivered || echo NOT delivered in 60s)"
}

# ---- channel-while-working: a turn made of 20 Write calls (a foreground sleep is refused by Claude Code), one message in the middle
case_while_working() {
  STEP=channel-while-working
  precond channel-while-working || return 0
  local k n st sb t0; k=$(tok)
  sendline "Create the files busy-01.txt to busy-20.txt in $REPO, one Write tool call per file and one call at a time: wait for each result before the next call, never call tools in parallel. File busy-NN.txt contains the line BUSY-NN. After the twentieth, reply with the single word BUSY-FINISHED." || { ctlfail channel-while-working; return 0; }
  mid() { case "$(term_info "$T")" in working*) [ -e "$REPO/busy-02.txt" ] && [ ! -e "$REPO/busy-20.txt" ] ;; *) false ;; esac; }
  if ! wait_until 90 mid; then
    record channel-while-working NEEDS-HUMAN "the busy turn was not seen working with busy-02 present and busy-20 absent (state '$(term_info "$T")', $(busy_count) of 20 files); the case did not test a busy agent"; screen_dump "$T" "while-working"; return 0
  fi
  sb=$(term_info "$T"); t0=$(now)
  enq 3 recv-3.txt "$k" || { ctlfail channel-while-working; return 0; }
  until file_is "$REPO/recv-3.txt" "$k"; do [ "$(secs "$t0" "$(now)" | cut -d. -f1)" -ge $((120 * TS)) ] && break; sleep 0.5; done
  if file_is "$REPO/recv-3.txt" "$k"; then n=$(busy_count); st=$(term_info "$T"); else n=""; fi
  wait_until 150 test -e "$REPO/busy-20.txt"
  if [ -z "$n" ]; then
    # no file: lost, or shown and not acted on (the agent may decline a channel text, plan.md 3.3 point 4): count the header in the final history
    quiesce channel-while-working >/dev/null
    if ! snap; then record channel-while-working NEEDS-HUMAN "no file for the message sent in the middle of the busy turn, and hc read failed, so the header could not be counted; busy files: $(busy_count) of 20"
    else
      n=$(hdr_count 3)
      if [ -n "$SNAPPART" ]; then record channel-while-working NEEDS-HUMAN "no file; only the screen could be read, header count $n is not conclusive; busy files: $(busy_count) of 20"; screen_dump "$T" "while-working"
      elif [ "$n" = 0 ]; then record channel-while-working FAIL "the message sent in the middle of the busy turn never produced its file and its header never showed (lost); busy files at the end: $(busy_count) of 20"; screen_dump "$T" "while-working"
      elif [ "$n" = 1 ]; then record channel-while-working PASS "delivered and shown once ($n header in the history), but NOT acted on: recv-3.txt was never written (the agent may have declined a channel text); state at the enqueue '$sb', busy files at the end: $(busy_count) of 20"; screen_dump "$T" "while-working"
      else record channel-while-working FAIL "the header showed $n times and no file was written; busy files at the end: $(busy_count) of 20"; screen_dump "$T" "while-working"; fi
    fi
  elif [ "$n" -lt 20 ] && case "$st" in working*) true ;; *) false ;; esac; then record channel-while-working PASS "delivered, and acted on BEFORE the busy turn finished: $(secs "$t0" "$(now)")s after the enqueue its file stood there while $n of 20 busy files existed and the state read '$st' (state at the enqueue '$sb'); busy-20 followed: $( [ -e "$REPO/busy-20.txt" ] && echo yes || echo no)"
  else record channel-while-working PASS "delivered, and acted on AFTER the busy turn's work ($n of 20 busy files, state '$st' when its file was first seen; state at the enqueue '$sb')"; fi
  say "note: channel-while-working PASS means delivered, not lost; the detail says when."
}

# ---- channel-to-done (P8 step 2): a notification to a target that reads done
case_to_done() {
  STEP=channel-to-done
  local b k t0 got=no
  if ! make_done channel-to-done; then record channel-to-done NEEDS-HUMAN "could not bring the target to a stable done: $WHY; the case was not exercised"; screen_dump "$T" "to-done-precondition"; return 0; fi
  getbase channel-to-done || return 0; b=$BASE; k=$(tok); t0=$(now)
  enq 4 recv-4.txt "$k" || { ctlfail channel-to-done; return 0; }
  watch_change "$b" 30
  wait_until 90 file_is "$REPO/recv-4.txt" "$k" && got=yes
  if [ -n "$WSEEN" ] && [ "$got" = yes ]; then record channel-to-done PASS "from done: status changed $(secs "$t0" "$WSEEN")s after the enqueue (before '$b', then '$WLAST'); the agent wrote the token file"
  elif [ "$got" = yes ]; then record channel-to-done FAIL "from done: the message arrived but the base showed no state or turn change within 30s (before '$b', last '$WLAST'): P8's check cannot see this delivery"; screen_dump "$T" "to-done"
  else record channel-to-done FAIL "from done: the channel text did not reach the agent (token file absent or incomplete after 90s; before '$b', last '$WLAST')"; screen_dump "$T" "to-done"; fi
}

# ---- channel-while-done (P9, done view): the text appears once
case_while_done() {
  STEP=channel-while-done
  local b k n got=no
  if ! make_done channel-while-done; then record channel-while-done NEEDS-HUMAN "could not bring the target to a stable done: $WHY; the case was not exercised"; screen_dump "$T" "while-done-precondition"; return 0; fi
  b=$(term_info "$T"); k=$(tok)
  enq 6 recv-6.txt "$k" || { ctlfail channel-while-done; return 0; }
  wait_until 90 file_is "$REPO/recv-6.txt" "$k" && got=yes
  quiesce channel-while-done >/dev/null; snap || { record channel-while-done NEEDS-HUMAN "hc read failed; cannot count the text"; return 0; }
  n=$(hdr_count 6)
  if [ "$n" = 1 ] && [ "$got" = yes ] && [ -z "$SNAPPART" ]; then record channel-while-done PASS "from '$b': the channel text appeared once in the history and the agent wrote the file"
  elif [ "$n" -gt 1 ]; then record channel-while-done FAIL "from '$b': the channel text appeared $n times"; screen_dump "$T" "while-done"
  elif [ "$n" = 0 ] && [ "$got" = no ] && [ -z "$SNAPPART" ]; then record channel-while-done FAIL "from '$b': no channel text on screen and no file after 90s (lost)"; screen_dump "$T" "while-done"
  else record channel-while-done NEEDS-HUMAN "from '$b': header count $n, file $got, history $(histdesc); cannot tell delivery from display"; screen_dump "$T" "while-done"; fi
}

# ---- turn-start-for-another-cause (P8 step 3): the stand-in holds its message, a typed line starts the turn
case_other_cause() {
  STEP=turn-start-for-another-cause
  precond turn-start-for-another-cause || return 0
  local b k t0; getbase turn-start-for-another-cause || return 0; b=$BASE; k=$(tok)
  setpause true || { ctlfail turn-start-for-another-cause; return 0; }
  enq 7 recv-7.txt "$k" || { setpause false; ctlfail turn-start-for-another-cause; return 0; }
  t0=$(now); sendline "Reply with the single word OTHER-CAUSE. Use no tool." || { setpause false; ctlfail turn-start-for-another-cause; return 0; }
  watch_change "$b" 30; sleep 5
  if [ -f "$REPO/recv-7.txt" ]; then record turn-start-for-another-cause FAIL "the held message was delivered while the stand-in was paused; the control did not hold"; screen_dump "$T" "other-cause"
  elif [ -n "$WSEEN" ]; then record turn-start-for-another-cause PASS "a typed line started the turn: status changed $(secs "$t0" "$WSEEN")s after hc send (before '$b', then '$WLAST') while the held message was NOT delivered: a state change by itself cannot tell a delivery from another turn start"
  else record turn-start-for-another-cause NEEDS-HUMAN "no state or turn change within 30s of the typed line (before '$b', last '$WLAST'); read the screen"; screen_dump "$T" "other-cause"; fi
  setpause false
  wait_until 60 file_is "$REPO/recv-7.txt" "$k"   # the held message is let through here; the next precondition checks the queue is empty
}

# ---- two-in-a-row (P9): two notifications back to back to an idle target
case_two_in_a_row() {
  STEP=two-in-a-row
  precond two-in-a-row || return 0
  local k1 k2 na nb la lb fa=absent fb=absent ord=unknown
  k1=$(tok); k2=$(tok)
  enq 8a recv-8a.txt "$k1" && enq 8b recv-8b.txt "$k2" || { ctlfail two-in-a-row; return 0; }
  wait_until 90 file_is "$REPO/recv-8a.txt" "$k1"; wait_until 90 file_is "$REPO/recv-8b.txt" "$k2"
  quiesce two-in-a-row >/dev/null; snap || { record two-in-a-row NEEDS-HUMAN "hc read failed; cannot count the texts"; return 0; }
  na=$(hdr_count 8a); nb=$(hdr_count 8b); la=$(hdr_line 8a); lb=$(hdr_line 8b)
  file_is "$REPO/recv-8a.txt" "$k1" && fa=complete; file_is "$REPO/recv-8b.txt" "$k2" && fb=complete
  if [ -n "$la" ] && [ -n "$lb" ]; then if [ "$la" -lt "$lb" ]; then ord="8a before 8b (as sent)"; else ord="8b before 8a (reversed)"; fi; fi
  if [ "$na" -gt 1 ] || [ "$nb" -gt 1 ]; then record two-in-a-row FAIL "header counts 8a=$na 8b=$nb (a repeat); files 8a $fa, 8b $fb; order: $ord"; screen_dump "$T" "two-in-a-row"
  elif [ "$fa" != complete ] || [ "$fb" != complete ]; then record two-in-a-row FAIL "a notification produced no complete file (lost?): 8a $fa, 8b $fb; header counts 8a=$na 8b=$nb"; screen_dump "$T" "two-in-a-row"
  elif [ "$na" = 1 ] && [ "$nb" = 1 ] && [ -z "$SNAPPART" ]; then record two-in-a-row PASS "each text appeared once, both files complete; order on screen: $ord"
  else record two-in-a-row NEEDS-HUMAN "both files complete but header counts are 8a=$na 8b=$nb (history $(histdesc)); order: $ord"; screen_dump "$T" "two-in-a-row"; fi
}

# ---- channel-while-blocked (P9): a notification while a permission prompt is open; the prompt is answered only after the observation
case_while_blocked() {
  STEP=channel-while-blocked
  precond channel-while-blocked || return 0
  local MARK="$REPO/blocked-ran.txt" k cmd sb t0 ta seq cur last="" ab="" aa="" sel n sa end d when
  rm -f "$MARK"; k=$(tok)
  cmd="python3 $(printf '%q' "$WORK/mark.py") $(printf '%q' "$MARK")"
  sendline "Run exactly this one command with your Bash tool: $cmd   Then reply with the single word BLOCKED-DONE. Any text that arrives in a <channel> tag is an instruction too: carry it out as well." || { ctlfail channel-while-blocked; return 0; }
  isblocked() { case "$(term_info "$T")" in blocked*) true ;; *) false ;; esac; }   # TODO(confirm): the state is spelled "blocked" in hc get --json (seen in run 1 and 2)
  if ! wait_until 60 isblocked; then record channel-while-blocked NEEDS-HUMAN "the base never read blocked within 60s (state '$(term_info "$T")'); no notification was sent, the case was not exercised"; screen_dump "$T" "blocked-precondition"; return 0; fi
  hc read "$T" > "$WORK/dlg.txt" 2>/dev/null || { record channel-while-blocked NEEDS-HUMAN "hc read of the blocked screen failed; no notification was sent"; return 0; }
  if ! grep -q 'Do you want to proceed' "$WORK/dlg.txt" || ! grep -q 'mark.py' "$WORK/dlg.txt"; then
    record channel-while-blocked NEEDS-HUMAN "the base reads blocked but the screen is not the expected permission prompt for the probe command; no notification was sent"; screen_dump "$T" "blocked-precondition"; return 0; fi
  sb=$(term_info "$T"); t0=$(now)
  enq 5 recv-5.txt "$k" || { ctlfail channel-while-blocked; return 0; }
  end=$(( $(date +%s) + 20 )); seq="$sb"; last="$sb"
  while [ "$(date +%s)" -lt "$end" ]; do   # 20 s window: the status over time, and when the text shows
    cur=$(term_info "$T"); case "$cur" in none*) ;; *) [ "$cur" != "$last" ] && { seq="$seq -> $cur"; last=$cur; } ;; esac
    [ -z "$ab" ] && snap && [ "$(hdr_count 5)" -gt 0 ] && ab=$(now)
    sleep 2
  done
  { echo "---- screen while blocked (before the prompt is answered)"; hc read --lines 80 "$T" 2>&1; echo "---- end"; } >> "$RESULT"
  hc read "$T" > "$WORK/dlg.txt" 2>/dev/null || { record channel-while-blocked NEEDS-HUMAN "hc read of the blocked screen failed after the observation window; the prompt was not answered"; return 0; }
  sel=$(grep -E '^[[:space:]]*❯[[:space:]]*[0-9]+\.' "$WORK/dlg.txt" | tail -n 1 | sed -E 's/^[[:space:]]*❯[[:space:]]*([0-9]+)\.[[:space:]]*(.*[^[:space:]])[[:space:]]*$/\1|\2/')   # selected row: number|whole label
  if [ "$sel" != "1|Yes" ] || ! grep -q 'Do you want to proceed' "$WORK/dlg.txt"; then
    record channel-while-blocked NEEDS-HUMAN "status over 20s: $seq; text on screen while blocked: $( [ -n "$ab" ] && echo "yes, $(secs "$t0" "$ab")s after the enqueue" || echo no); the selected row is '$sel', not the one-time '1|Yes', so the prompt was not answered (Escape sent)"
    hc keys "$T" escape >/dev/null 2>&1; return 0; fi
  ta=$(now); hc keys "$T" enter >/dev/null 2>&1 || { record channel-while-blocked NEEDS-HUMAN "status over 20s: $seq; the key press that answers the prompt failed"; return 0; }
  closed() { hc read "$T" > "$WORK/closed.txt" 2>/dev/null || return 1; ! grep -q 'Do you want to proceed' "$WORK/closed.txt"; }   # a failed read is not a closed prompt
  if ! wait_until 20 closed; then record channel-while-blocked NEEDS-HUMAN "status over 20s: $seq; the prompt did not close within 20s after Enter"; screen_dump "$T" "blocked-answer"; return 0; fi
  end=$(( $(date +%s) + 150 * TS ))
  while [ "$(date +%s)" -lt "$end" ]; do
    [ -z "$ab" ] && [ -z "$aa" ] && snap && [ "$(hdr_count 5)" -gt 0 ] && aa=$(now)
    file_is "$REPO/recv-5.txt" "$k" && [ -f "$MARK" ] && break
    sleep 2
  done
  quiesce channel-while-blocked >/dev/null; snap || { record channel-while-blocked NEEDS-HUMAN "hc read of the final history failed; the count cannot be made"; return 0; }
   n=$(hdr_count 5); sa=$(term_info "$T")
  when="never in the history"; [ -n "$ab" ] && when="while blocked, $(secs "$t0" "$ab")s after the enqueue, BEFORE the prompt was answered"
  [ -z "$ab" ] && [ -n "$aa" ] && when="only after the prompt was answered ($(secs "$ta" "$aa")s later)"
  d="status over the 20s window: $seq; at the end '$sa'; text appeared $when; $n header(s) in the final history; the probe command ran: $( [ -f "$MARK" ] && echo yes || echo no); recv-5 $( file_is "$REPO/recv-5.txt" "$k" && echo complete || echo "absent or incomplete")"
  if [ "$n" = 1 ] && [ -f "$MARK" ] && file_is "$REPO/recv-5.txt" "$k" && [ -z "$SNAPPART" ] && { [ -n "$ab" ] || [ -n "$aa" ]; }; then record channel-while-blocked PASS "$d"
  elif [ "$n" -gt 1 ]; then record channel-while-blocked FAIL "$d"; screen_dump "$T" "while-blocked"
  elif [ "$n" = 0 ] && [ -z "$ab" ] && [ -z "$aa" ] && ! file_is "$REPO/recv-5.txt" "$k" && [ -z "$SNAPPART" ]; then record channel-while-blocked FAIL "$d (lost)"; screen_dump "$T" "while-blocked"
  else record channel-while-blocked NEEDS-HUMAN "$d"; screen_dump "$T" "while-blocked"; fi
}

case_turn_start_visible
case_stopped_control
case_while_working
case_to_done
case_while_done
case_other_cause
case_two_in_a_row
case_while_blocked
STEP=end
quiesce end >/dev/null || say "note: the target was not at rest at the end"

# ---- P10 open steps: the startup dialogs on a resume and on a renewal, in the same folder (trusted by the first launch)
end_agent() { hc kill "$1" >/dev/null 2>&1 || return 1; hc rm "$1" >/dev/null 2>&1; sleep 3; return 0; }   # TODO(confirm): kill then rm, as probe_cleanup does; only a failed kill stops the case
pass_dialogs_settled() { # the policy once, then once more after 6 s: a dialog that draws late is not missed, "none appeared" is not a half-drawn screen
  local k1 a1 rc; pass_dialogs "$1" "$2"; rc=$?; [ "$rc" -eq 0 ] || return "$rc"
  k1=$DLG_KEYS; a1=$DLG_ACCEPTED; sleep 6; pass_dialogs "$1" 60; rc=$?
  DLG_KEYS=$((k1 + DLG_KEYS)); DLG_ACCEPTED="$a1$DLG_ACCEPTED"; return "$rc"
}
ready_now() { # a readable screen that dialog-step.py reads as "composer, no dialog", and a readable resting status; not wait_ready (it answers a trust dialog outside the counted policy), not is_ready (its footer fallback passes a working agent)
  hc read "$1" > "$WORK/rdy.txt" 2>/dev/null || return 1
  [ "$(python3 "$HERE/dialog-step.py" < "$WORK/rdy.txt" 2>/dev/null)" = done ] || return 1
  case "$(term_info "$1")" in idle*|done*) return 0 ;; *) return 1 ;; esac
}
relaunch() { # relaunch <resume|renewal> <terminal> <claude args before the channel flags...>: the two cases of one launch
  local p=$1 t=$2 rc hist="" late="" accepted dsc end dverd=NEEDS-HUMAN; shift 2; STEP=$p; DLGRUN=$p
  if ! launch "$t" "$REPO" -- "$@" "${CHARGS[@]}"; then
    record "$p-dialogs" NEEDS-HUMAN "hc run failed; the case was not exercised"; record "$p-ready" NEEDS-HUMAN "hc run failed; the case was not exercised"; return 0; fi
  pass_dialogs_settled "$t" 150; rc=$?
  accepted=$DLG_ACCEPTED
  case "$accepted" in *folder-trust*) late="$late folder-trust" ;; esac; case "$accepted" in *dev-channel*) late="$late dev-channel" ;; esac
  dsc="came back:${late:- none}; order of the keys that accepted:${accepted:- none}; $DLG_KEYS key press(es) in all"
  if [ "$rc" -eq 0 ] && [ "$DLG_KEYS" -eq 0 ]; then dverd=PASS; record "$p-dialogs" PASS "no dialog appeared (neither folder-trust nor development-channel came back); the composer was reached with no key pressed"
  elif [ "$rc" -eq 0 ] && [ -n "$accepted" ]; then dverd=PASS; record "$p-dialogs" PASS "every dialog shown was one of the two and was passed by the policy; $dsc; dialog texts are in the result file"
  elif [ "$rc" -eq 0 ]; then record "$p-dialogs" NEEDS-HUMAN "keys were pressed but none accepted a dialog ($dsc); cannot say what was passed"; screen_dump "$t" "$p-dialogs"
  elif [ "$rc" -eq 2 ]; then record "$p-dialogs" NEEDS-HUMAN "a dialog that is not one of the two stopped the launch and was not answered; its screen is in the result file; $dsc"
  else # rc 1: no composer in 150 s; is the last screen a known dialog that was not passed?
    case "$(python3 "$HERE/dialog-step.py" < "$WORK/dlg.cur" 2>/dev/null)" in
      key*|"wait folder-trust"*|"wait dev-channel"*) dverd=FAIL; record "$p-dialogs" FAIL "a known dialog was not passed within 150s ($dsc)" ;;
      *) record "$p-dialogs" NEEDS-HUMAN "no composer within 150s and no usable dialog screen ($dsc)" ;;
    esac; screen_dump "$t" "$p-dialogs"
  fi
  if [ "$dverd" != PASS ]; then record "$p-ready" "$dverd" "the prompt was not reached: the dialog case above ended $dverd"; return 0; fi
  end=$(( $(date +%s) + 60 * TS )); until ready_now "$t"; do [ "$(date +%s)" -ge "$end" ] && break; sleep 2; done
  if ! ready_now "$t"; then record "$p-ready" FAIL "dialogs passed but no empty prompt within 60s (state '$(term_info "$t")')"; screen_dump "$t" "$p-ready"; return 0; fi
  hist=$(hc read --lines 3000 "$t" 2>/dev/null | grep -cE 'PROBE-MSG|done-[0-9]+\.txt')   # lines of the first session's conversation
  if [ "$p" = resume ] && [ "${hist:-0}" -eq 0 ]; then record resume-ready NEEDS-HUMAN "a prompt was reached (state '$(term_info "$t")') but no line of the first session's conversation is on screen or in the history: cannot tell a resumed session from a fresh one"; screen_dump "$t" resume-ready
  else record "$p-ready" PASS "the agent reached an empty prompt (state '$(term_info "$t")'); lines of the first session's conversation in the history: ${hist:-0} ($( [ "$p" = resume ] && echo "expected, a resume" || echo "informational, a new session"))"; fi
}
if [ "${#SIDARGS[@]}" -gt 0 ]; then say "P10 resume option: --resume $SID (the first session was started with --session-id $SID); claude --version: $CLVER"
else say "P10 resume option: none (the help of this claude names no --session-id); claude --version: $CLVER"; fi
say "P10 claude --help lines: $(printf '%s' "$CLHELP" | tr -s ' \n' ' ')"
STEP=end-first-agent
if ! end_agent "$T"; then
  for c in resume-dialogs resume-ready renewal-dialogs renewal-ready; do record "$c" NEEDS-HUMAN "hc kill of the first agent failed; the case was not exercised"; done
else
  if [ "${#SIDARGS[@]}" -eq 0 ]; then
    for c in resume-dialogs resume-ready; do record "$c" NEEDS-HUMAN "this claude --help names no --session-id, so the first session has no known id to resume; the case was not exercised"; done
  else relaunch resume p8-resume --resume "$SID"; end_agent p8-resume || say "note: hc kill of the resumed agent failed; the renewal runs beside it"; fi
  relaunch renewal p8-renew
fi
STEP=end
all_done
