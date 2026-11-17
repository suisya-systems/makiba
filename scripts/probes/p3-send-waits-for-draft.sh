#!/usr/bin/env bash
# P3 (probes.md): a send waits for a person's unfinished typing (herdr-core 0.1.20 behaviour).
# Refuses to run on hc below 0.1.20.
# Run from a login shell: bash scripts/probes/p3-send-waits-for-draft.sh <result-dir>
# P3_MODE=keys   (default) the half draft is typed with hc keys; this STANDS IN for a person.
#                Whether the base treats keys as a person's typing is itself unmeasured, so a
#                result of this mode is labelled "stand-in" in the result file.
# P3_MODE=human  the script prints the attach command; a person types "half typed" (no Enter)
#                in the attached terminal, presses Enter in this shell, and later presses Enter
#                in the attached terminal when asked.
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P3: send waits for unfinished typing (hc >= 0.1.20)"; exit 0 ;; esac
probe_init p3 "${1:-}" 0.1.20

MODE=${P3_MODE:-keys}
REPO=$WORK/repo; new_repo "$REPO" || { say "RESULT p3: precondition-failed (repository)"; exit 2; }
T=p3-agent
launch "$T" "$REPO" -- --permission-mode default || { record launch FAIL "hc run failed"; finish; }
wait_ready "$T" 120 || { record launch NEEDS-HUMAN "claude did not reach an empty prompt in 120s"; screen_dump "$T" launch; finish; }

if [ "$MODE" = human ]; then
  say "attach with:  HC_SESSION=$SESSION XDG_CONFIG_HOME=$XDG_CONFIG_HOME XDG_STATE_HOME=$XDG_STATE_HOME XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR $HC_BIN attach $T"
  say "type  half typed  (no Enter), then press Enter HERE."
  read -r _
else
  # TODO(confirm): the key name "space" for hc keys (help lists enter, esc, tab, up, ctrl+c, a, ...).
  hc keys "$T" h a l f space t y p e d >/dev/null 2>&1
  sleep 2
fi
if ! hc read "$T" 2>/dev/null | grep -q 'half typed'; then
  record draft-visible NEEDS-HUMAN "the draft 'half typed' is not on the screen; use P3_MODE=human"
  screen_dump "$T" "after typing"; finish
fi

MSG=$(printf '<makiba from="probe" kind="message" id="1">\nPROBE-BODY\n</makiba>')
S0=$(date +%s)
( "$HC_BIN" send "$T" "$MSG" > "$WORK/send.out" 2>&1; echo $? > "$WORK/send.rc"; date +%s > "$WORK/send.end" ) &
sleep 20
scr=$(hc read "$T" 2>/dev/null)
rc=$(cat "$WORK/send.rc" 2>/dev/null || echo running)
if printf '%s' "$scr" | grep -q 'PROBE-BODY'; then
  record send-held-20s FAIL "wrapped text reached the screen while the draft was there (send rc=$rc, out: $(head -c 200 "$WORK/send.out" 2>/dev/null | tr '\n' ' '))"
  screen_dump "$T" "after 20s"
elif [ "$rc" != running ] && [ "$rc" != 0 ]; then
  record send-held-20s NEEDS-HUMAN "hc send exited $rc before 20s (out: $(head -c 200 "$WORK/send.out" 2>/dev/null | tr '\n' ' ')); a failed send says nothing about waiting"
  screen_dump "$T" "after 20s"
elif ! printf '%s' "$scr" | grep -q 'half typed'; then
  record send-held-20s NEEDS-HUMAN "the draft is gone from the screen without the wrapped text; read the screen"
  screen_dump "$T" "after 20s"
else
  record send-held-20s PASS "after 20s the draft is intact and the wrapped text is not typed (send: $rc)"
fi

if [ "$MODE" = human ]; then say "NOW press Enter in the attached terminal, then press Enter HERE."; read -r _
else hc keys "$T" enter >/dev/null 2>&1; fi

i=0; while [ "$i" -lt 90 ]; do
  hc read --lines 300 "$T" 2>/dev/null | grep -q 'PROBE-BODY' && [ -f "$WORK/send.rc" ] && break
  i=$((i + 3)); sleep 3
done
wait=$(( $(cat "$WORK/send.end" 2>/dev/null || date +%s) - S0 ))
hist=$(hc read --lines 300 "$T" 2>&1)
# Prompt boundaries: each prompt starts with the composer marker. TODO(confirm): the marker is "❯" (paddock's ready check uses it).
dl=$(printf '%s\n' "$hist" | grep -nE '^[[:space:]]*[❯>] *half typed' | head -n 1 | cut -d: -f1)
bl=$(printf '%s\n' "$hist" | grep -nE '^[[:space:]]*[❯>] *<makiba' | head -n 1 | cut -d: -f1)
if printf '%s\n' "$hist" | grep 'half typed' | grep -Eq 'makiba|PROBE-BODY'; then
  record order-after-enter FAIL "the draft and the wrapped text are one prompt (joined); send rc=$(cat "$WORK/send.rc" 2>/dev/null) after ${wait}s"
  screen_dump "$T" "after Enter"
elif [ -n "$dl" ] && [ -n "$bl" ] && [ "$dl" -lt "$bl" ]; then
  record order-after-enter PASS "'half typed' and the wrapper each start their own prompt, draft first; send returned rc=$(cat "$WORK/send.rc" 2>/dev/null) after ${wait}s"
else
  record order-after-enter NEEDS-HUMAN "no separate prompt line for both (draft prompt line ${dl:-none}, wrapper prompt line ${bl:-none}); read the screen"
  screen_dump "$T" "after Enter"
fi
say "note: mode=$MODE$( [ "$MODE" = keys ] && echo ' (stand-in: keys typed by the script, not a person)')"
finish
