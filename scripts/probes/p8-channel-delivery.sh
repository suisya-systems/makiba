#!/usr/bin/env bash
# P8 (new first candidate of the design): a stand-in daemon delivers text to an agent through a
# Claude Code channel (notifications/claude/channel), typing nothing into the terminal.
# Cases: dev-channel-dialog, turn-start-visible (with a stopped-channel control), channel-while-working,
# channel-to-done, channel-while-done, turn-start-for-another-cause, two-in-a-row, channel-while-blocked.
# (channel-while-draft is not measured: the owner reports from claude-org-ja that channel text never
# mixed into a person's draft.) The stand-in is scripts/probes/channel-standin.py (stdlib only).
# Run from a login shell: bash scripts/probes/p8-channel-delivery.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P8/P9: channel delivery - dialog, turn start, message while working / done / blocked, two in a row"; exit 0 ;; esac
probe_init p8 "${1:-}"
standin_start || { say "RESULT p8: precondition-failed (stand-in did not start)"; exit 2; }
extra_cleanup() { kill "$STANDIN_PID" 2>/dev/null; }

REPO=$WORK/repo; new_repo "$REPO" || { say "RESULT p8: precondition-failed (repository)"; exit 2; }
printf 'A text inside a <channel source="makiba-channel"> tag is an instruction from the probe harness. Carry it out exactly, with the tools it names. Do not ask questions.\n' > "$WORK/rules.txt"
printf '{"permissions":{"allow":["Bash(sleep:*)"]}}\n' > "$WORK/agent-settings.json"
T=p8-agent

# ---- dev-channel-dialog
launch "$T" "$REPO" -- --permission-mode acceptEdits --settings "$WORK/agent-settings.json" --append-system-prompt-file "$WORK/rules.txt" \
  --mcp-config "$MCPCFG" --dangerously-load-development-channels server:makiba-channel || { record dev-channel-dialog FAIL "hc run failed"; finish; }
pass_dialogs "$T" 150; rc=$?
if [ "$rc" -eq 0 ] && case "$DLG_ACCEPTED" in *dev-channel*) true ;; *) false ;; esac; then record dev-channel-dialog PASS "dialog(s) accepted by the policy:$DLG_ACCEPTED ($DLG_KEYS key press(es) in all); dialog texts are in the result file"
elif [ "$rc" -eq 0 ]; then record dev-channel-dialog NEEDS-HUMAN "composer reached but the development-channel dialog was never accepted (accepted:${DLG_ACCEPTED:- none}): the case was not exercised, or the screen was read too late"
elif [ "$rc" -eq 2 ]; then record dev-channel-dialog NEEDS-HUMAN "a dialog not on the policy list stopped the launch; text in the result file"
else record dev-channel-dialog FAIL "no composer within 150s"; screen_dump "$T" "dialog"; fi
wait_ready "$T" 60 || { record launch NEEDS-HUMAN "agent not ready"; screen_dump "$T" launch; finish; }
sleep 3   # the channel server registers after the composer is up

tok() { python3 -c 'import secrets;print("TOK-"+secrets.token_hex(4))'; }
msg() { printf 'PROBE-MSG %s: use the Write tool to create the file %s in your working folder, containing exactly the text %s, then reply with the single word GOT.' "$1" "$2" "$3"; }
secs() { python3 -c 'import sys;print(round(float(sys.argv[2])-float(sys.argv[1]),1))' "$1" "$2"; }
now() { date +%s.%N; }

# ---- turn-start-visible, with the stopped-channel control
wait_idle "$T" 30
b=$(term_info "$T"); k=$(tok)
t0=$(now); enqueue "$(msg 2 recv-2.txt "$k")" >/dev/null
seen=""; i=0
while [ "$i" -lt 100 ]; do   # 100 x 0.3 s
  a=$(term_info "$T")
  if [ "${a%% *}" = working ] || [ "${a#* }" != "${b#* }" ]; then seen=$(now); break; fi
  i=$((i + 1)); sleep 0.3
done
j=0; while [ ! -f "$REPO/recv-2.txt" ] && [ "$j" -lt 40 ]; do j=$((j + 1)); sleep 3; done
got=no; [ -f "$REPO/recv-2.txt" ] && grep -q "$k" "$REPO/recv-2.txt" && got=yes
if [ -n "$seen" ] && [ "$got" = yes ]; then record turn-start-visible PASS "status changed $(secs "$t0" "$seen")s after the enqueue (before: '$b', then: '$a'); the agent wrote the token file"
elif [ "$got" = yes ]; then record turn-start-visible FAIL "the message arrived but the base showed no state or turn change within 30s (before '$b', last '$a')"; screen_dump "$T" "turn-start"
else record turn-start-visible FAIL "the channel text did not reach the agent (token file absent after 120s)"; screen_dump "$T" "turn-start"; fi
wait_idle "$T" 60
b=$(term_info "$T"); k=$(tok)
admin '{"op":"pause","value":true}' >/dev/null; enqueue "$(msg 2b recv-2b.txt "$k")" >/dev/null
sleep 20; a=$(term_info "$T")
if [ "$a" = "$b" ] && [ ! -f "$REPO/recv-2b.txt" ]; then record turn-start-unchanged-without-delivery PASS "channel stopped: status stayed '$a' for 20s and no file appeared"
else record turn-start-unchanged-without-delivery FAIL "channel stopped but status went '$b' -> '$a' or a file appeared"; screen_dump "$T" "stopped-channel"; fi
admin '{"op":"pause","value":false}' >/dev/null
j=0; while [ ! -f "$REPO/recv-2b.txt" ] && [ "$j" -lt 20 ]; do j=$((j + 1)); sleep 3; done
say "note: after the channel was resumed the held message was $( [ -f "$REPO/recv-2b.txt" ] && echo delivered || echo NOT delivered in 60s)"
wait_idle "$T" 60

# ---- channel-while-working
k=$(tok)
hc send "$T" "Run this with your Bash tool: sleep 45. Then use the Write tool to create long-done.txt containing LONGDONE. Then reply with the single word LONG-FINISHED." >/dev/null 2>&1
j=0; while [ "$j" -lt 60 ]; do case "$(term_info "$T")" in working*) break ;; esac; j=$((j + 1)); sleep 0.5; done
LONGON=""; [ "$j" -lt 60 ] && LONGON=1
sleep 8
case "$(term_info "$T")" in working*) ;; *) LONGON="" ;; esac
[ -f "$REPO/long-done.txt" ] && LONGON=""
enqueue "$(msg 3 recv-3.txt "$k")" >/dev/null
j=0; while { [ ! -f "$REPO/recv-3.txt" ] || [ ! -f "$REPO/long-done.txt" ]; } && [ "$j" -lt 80 ]; do j=$((j + 1)); sleep 3; done
wait_idle "$T" 60; sleep 5
if [ -z "$LONGON" ]; then record channel-while-working NEEDS-HUMAN "the long turn was not seen running when the message was enqueued (state not working, or already finished); the case did not test a busy agent"; screen_dump "$T" "while-working"
elif [ ! -f "$REPO/recv-3.txt" ]; then record channel-while-working FAIL "the message sent during a long turn never produced its file (lost?); long-done.txt $( [ -f "$REPO/long-done.txt" ] && echo present || echo absent)"; screen_dump "$T" "while-working"
elif [ ! -f "$REPO/long-done.txt" ]; then record channel-while-working NEEDS-HUMAN "the message arrived but the long turn's own file is missing; read the screen"; screen_dump "$T" "while-working"
elif python3 - "$REPO/recv-3.txt" "$REPO/long-done.txt" <<'PY'
import os, sys
sys.exit(0 if os.stat(sys.argv[1]).st_mtime < os.stat(sys.argv[2]).st_mtime else 1)
PY
then record channel-while-working PASS "delivered, and acted on BEFORE the long turn finished (it interrupted the turn at a tool boundary)"
else record channel-while-working PASS "delivered, and acted on AFTER the long turn finished (it waited for the turn's end)"; fi
say "note: channel-while-working PASS means delivered, not lost; the detail says when. A message acted on before the long file was written interrupted the turn."
# ---- cases below: P8 step 2 and 3, P9 done / two in a row / blocked (added for Issue 95; not run in the sandbox that wrote them)
# Each case re-establishes its own state (resumed channel, finished turn) and ends in NEEDS-HUMAN when it cannot.
seen_count() { hc read --lines 3000 "$T" 2>/dev/null | grep -c "PROBE-MSG $1:"; }   # times the channel text shows in the history; TODO(confirm): the tag text is shown on screen
seen_line() { hc read --lines 3000 "$T" 2>/dev/null | grep -n -m1 "PROBE-MSG $1:" | cut -d: -f1; }
wait_file() { local j=0; while [ ! -f "$1" ] && [ "$j" -lt "$2" ]; do j=$((j + 1)); sleep 2; done; [ -f "$1" ]; }   # <file> <polls of 2 s>
watch_change() { # watch_change <before "state turn"> <seconds>: sets WSEEN (time) and WLAST; WSEEN empty if no state or turn change
  WSEEN=""; WLAST=$1; local i=0 a
  while [ "$i" -lt "$(( $2 * 10 / 3 ))" ]; do   # one look per 0.3 s
    a=$(term_info "$T")
    case "$a" in none*) i=$((i + 1)); sleep 0.3; continue ;; esac   # status unreadable: not a change
    WLAST=$a
    if [ "${a%% *}" = working ] || [ "${a#* }" != "${1#* }" ]; then WSEEN=$(now); return 0; fi
    i=$((i + 1)); sleep 0.3
  done
  return 1
}
idle_or() { # idle_or <case>: records NEEDS-HUMAN and fails when the target is not idle/done, so nothing is sent into a busy or blocked agent
  wait_idle "$T" 60 && case "$(term_info "$T")" in none*) false ;; esac && return 0
  record "$1" NEEDS-HUMAN "the target was not idle (state '$(term_info "$T")') after 60s, so the case was not exercised"; screen_dump "$T" "$1-precondition"; return 1
}
DONEN=0
make_done() { # a short turn, then the base reads done: needs the file AND the state, so a stale done is not taken for the end of this turn
  DONEN=$((DONEN + 1)); wait_idle "$T" 60 || return 1
  rm -f "$REPO/done-$DONEN.txt"
  hc send "$T" "Use the Write tool to create done-$DONEN.txt containing OK. Then reply with the single word OK." >/dev/null 2>&1
  wait_file "$REPO/done-$DONEN.txt" 45 || return 1
  local i=0; while [ "$i" -lt 30 ]; do case "$(term_info "$T")" in done*) sleep 2; return 0 ;; esac; i=$((i + 1)); sleep 1; done
  return 1
}

# ---- channel-to-done (P8 step 2): a notification to a target that reads done
admin '{"op":"pause","value":false}' >/dev/null
if make_done; then
  b=$(term_info "$T"); k=$(tok); t0=$(now); enqueue "$(msg 4 recv-4.txt "$k")" >/dev/null
  watch_change "$b" 30; wait_file "$REPO/recv-4.txt" 60
  got=no; [ -f "$REPO/recv-4.txt" ] && grep -q "$k" "$REPO/recv-4.txt" && got=yes
  if [ -n "$WSEEN" ] && [ "$got" = yes ]; then record channel-to-done PASS "from done: status changed $(secs "$t0" "$WSEEN")s after the enqueue (before '$b', then '$WLAST'); the agent wrote the token file"
  elif [ "$got" = yes ]; then record channel-to-done FAIL "from done: the message arrived but the base showed no state or turn change within 30s (before '$b', last '$WLAST'): P8's check cannot see this delivery"; screen_dump "$T" "to-done"
  else record channel-to-done FAIL "from done: the channel text did not reach the agent (token file absent after 120s; before '$b', last '$WLAST')"; screen_dump "$T" "to-done"; fi
else record channel-to-done NEEDS-HUMAN "could not bring the target to a finished turn that reads done (state '$(term_info "$T")'); the case was not exercised"; screen_dump "$T" "to-done-precondition"; fi
wait_idle "$T" 60

# ---- channel-while-done (P9, done view): the text appears once
if make_done; then
  b=$(term_info "$T"); k=$(tok); enqueue "$(msg 6 recv-6.txt "$k")" >/dev/null
  wait_file "$REPO/recv-6.txt" 60; wait_idle "$T" 60; sleep 3
  n=$(seen_count 6)
  if [ "$n" = 1 ] && [ -f "$REPO/recv-6.txt" ]; then record channel-while-done PASS "from '$b': the text appeared once in the history and the agent wrote the file"
  elif [ "$n" -gt 1 ]; then record channel-while-done FAIL "from '$b': the text appeared $n times"; screen_dump "$T" "while-done"
  elif [ ! -f "$REPO/recv-6.txt" ] && [ "$n" = 0 ]; then record channel-while-done FAIL "from '$b': no text on screen and no file after 120s (lost)"; screen_dump "$T" "while-done"
  else record channel-while-done NEEDS-HUMAN "from '$b': text count $n, file $( [ -f "$REPO/recv-6.txt" ] && echo present || echo absent); cannot tell delivery from display"; screen_dump "$T" "while-done"; fi
else record channel-while-done NEEDS-HUMAN "could not bring the target to a finished turn that reads done (state '$(term_info "$T")'); the case was not exercised"; screen_dump "$T" "while-done-precondition"; fi
wait_idle "$T" 60

# ---- turn-start-for-another-cause (P8 step 3): the stand-in holds its message, a typed line starts the turn
if idle_or turn-start-for-another-cause; then
b=$(term_info "$T"); k=$(tok)
admin '{"op":"pause","value":true}' >/dev/null; enqueue "$(msg 7 recv-7.txt "$k")" >/dev/null
t0=$(now); hc send "$T" "Reply with the single word OTHER-CAUSE. Use no tool." >/dev/null 2>&1
watch_change "$b" 30; sleep 5
if [ -n "$WSEEN" ] && [ ! -f "$REPO/recv-7.txt" ]; then record turn-start-for-another-cause PASS "a typed line started the turn: status changed $(secs "$t0" "$WSEEN")s after hc send (before '$b', then '$WLAST') while the held message was NOT delivered: a state change by itself cannot tell a delivery from another turn start"
elif [ -f "$REPO/recv-7.txt" ]; then record turn-start-for-another-cause FAIL "the held message was delivered while the stand-in was paused; the control did not hold"; screen_dump "$T" "other-cause"
else record turn-start-for-another-cause NEEDS-HUMAN "no state or turn change within 30s of the typed line (before '$b', last '$WLAST'); read the screen"; screen_dump "$T" "other-cause"; fi
wait_idle "$T" 60
admin '{"op":"pause","value":false}' >/dev/null
wait_file "$REPO/recv-7.txt" 30; wait_idle "$T" 60   # the held message is let through, so it cannot disturb the next case
fi

# ---- two-in-a-row (P9): two notifications back to back to an idle target
if idle_or two-in-a-row; then
k1=$(tok); k2=$(tok)
enqueue "$(msg 8a recv-8a.txt "$k1")" >/dev/null; enqueue "$(msg 8b recv-8b.txt "$k2")" >/dev/null
wait_file "$REPO/recv-8a.txt" 60; wait_file "$REPO/recv-8b.txt" 60; wait_idle "$T" 60; sleep 3
na=$(seen_count 8a); nb=$(seen_count 8b); la=$(seen_line 8a); lb=$(seen_line 8b)
fa=absent; fb=absent; [ -f "$REPO/recv-8a.txt" ] && fa=present; [ -f "$REPO/recv-8b.txt" ] && fb=present
ord=unknown
if [ -n "$la" ] && [ -n "$lb" ]; then if [ "$la" -lt "$lb" ]; then ord="8a before 8b (as sent)"; else ord="8b before 8a (reversed)"; fi; fi
if [ "$na" = 1 ] && [ "$nb" = 1 ] && [ "$fa" = present ] && [ "$fb" = present ]; then record two-in-a-row PASS "each text appeared once, both files written; order on screen: $ord"
elif [ "$na" -gt 1 ] || [ "$nb" -gt 1 ]; then record two-in-a-row FAIL "text counts 8a=$na 8b=$nb (a repeat); files 8a $fa, 8b $fb; order: $ord"; screen_dump "$T" "two-in-a-row"
elif [ "$fa" = absent ] || [ "$fb" = absent ]; then record two-in-a-row FAIL "a notification produced no file (lost?): 8a $fa, 8b $fb; text counts 8a=$na 8b=$nb"; screen_dump "$T" "two-in-a-row"
else record two-in-a-row NEEDS-HUMAN "both files written but text counts are 8a=$na 8b=$nb; order: $ord"; screen_dump "$T" "two-in-a-row"; fi
fi

# ---- channel-while-blocked (P9): a notification while a permission prompt is open; the prompt is answered only after the observation
if idle_or channel-while-blocked; then
rm -f "$REPO/blocked-ran.txt"; k=$(tok)
hc send "$T" "Run this with your Bash tool: python3 -c \"open('blocked-ran.txt','w').write('x')\" . Use no other tool. Then reply with the single word BLOCKED-DONE." >/dev/null 2>&1
j=0; while [ "$j" -lt 60 ]; do case "$(term_info "$T")" in blocked*) break ;; esac; j=$((j + 1)); sleep 1; done   # TODO(confirm): the state is spelled "blocked" in hc get --json
if [ "$j" -ge 60 ]; then
  record channel-while-blocked NEEDS-HUMAN "the base never read blocked within 60s of the command that settings do not allow (state '$(term_info "$T")'); no notification was sent, the case was not exercised"; screen_dump "$T" "blocked-precondition"
else
  sb=$(term_info "$T"); t0=$(now); enqueue "$(msg 5 recv-5.txt "$k")" >/dev/null
  ab=""; i=0; while [ "$i" -lt 20 ]; do [ "$(seen_count 5)" -gt 0 ] && { ab=$(now); break; }; i=$((i + 1)); sleep 1; done
  sd=$(term_info "$T"); { echo "---- screen while blocked (before the prompt is answered)"; hc read --lines 80 "$T" 2>&1; echo "---- end"; } >> "$RESULT"
  line=$(hc read "$T" 2>/dev/null | grep -E '^[[:space:]]*❯[[:space:]]*[0-9]+[.)]' | tail -n 1)   # only a numbered option row, never a submitted prompt line; TODO(confirm): the cursor sits on the one-time yes of a permission prompt
  case "$line" in *Yes*|*yes*) case "$line" in *"don't ask"*|*"always"*) ok="" ;; *) ok=1 ;; esac ;; *) ok="" ;; esac
  if [ -z "$ok" ]; then
    record channel-while-blocked NEEDS-HUMAN "blocked ('$sb'); text on screen while blocked: $( [ -n "$ab" ] && echo "yes after $(secs "$t0" "$ab")s" || echo "no in 20s"); the cursor line '$line' is not a one-time yes, so the prompt was not answered; screen text in the result file"
    hc keys "$T" escape >/dev/null 2>&1   # TODO(confirm): Escape declines the prompt
  else
    t1=$(now); hc keys "$T" enter >/dev/null 2>&1
    aa=""; i=0; while [ "$i" -lt 90 ]; do
      [ -z "$ab" ] && [ -z "$aa" ] && [ "$(seen_count 5)" -gt 0 ] && aa=$(now)
      [ -f "$REPO/recv-5.txt" ] && [ -f "$REPO/blocked-ran.txt" ] && break
      i=$((i + 1)); sleep 1
    done
    wait_idle "$T" 60; sleep 3; n=$(seen_count 5); sa=$(term_info "$T")
    when="never"; [ -n "$ab" ] && when="at once, $(secs "$t0" "$ab")s after the enqueue, BEFORE the prompt was answered"
    [ -z "$ab" ] && [ -n "$aa" ] && when="only after the prompt was answered ($(secs "$t1" "$aa")s later)"
    d="status at enqueue '$sb', after 20s '$sd', at the end '$sa'; text appeared $when; $n time(s) in the history; command ran: $( [ -f "$REPO/blocked-ran.txt" ] && echo yes || echo no); file recv-5 $( [ -f "$REPO/recv-5.txt" ] && echo present || echo absent)"
    if [ "$n" = 1 ] && [ -f "$REPO/recv-5.txt" ]; then record channel-while-blocked PASS "$d"
    elif [ "$n" -gt 1 ]; then record channel-while-blocked FAIL "$d"; screen_dump "$T" "while-blocked"
    elif [ "$n" = 0 ] && [ ! -f "$REPO/recv-5.txt" ]; then record channel-while-blocked FAIL "$d (lost)"; screen_dump "$T" "while-blocked"
    else record channel-while-blocked NEEDS-HUMAN "$d"; screen_dump "$T" "while-blocked"; fi
  fi
fi
fi

wait_idle "$T" 60
finish
