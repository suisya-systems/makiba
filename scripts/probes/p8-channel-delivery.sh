#!/usr/bin/env bash
# P8 (new first candidate of the design): a stand-in daemon delivers text to an agent through a
# Claude Code channel (notifications/claude/channel), typing nothing into the terminal.
# Cases: dev-channel-dialog, turn-start-visible (with a stopped-channel control), channel-while-working.
# (channel-while-draft is not measured: the owner reports from claude-org-ja that channel text never
# mixed into a person's draft.) The stand-in is scripts/probes/channel-standin.py (stdlib only).
# Run from a login shell: bash scripts/probes/p8-channel-delivery.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P8: channel delivery - dialog, turn start visible, message during a long turn"; exit 0 ;; esac
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
finish
