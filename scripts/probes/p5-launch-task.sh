#!/usr/bin/env bash
# P5 (probes.md): a task given in the launch arguments starts after the startup dialog.
# A fresh git repository no person has trusted; claude is launched through the base with the
# task as its last argument; the trust dialog is passed with the folder-trust policy; nothing
# is ever sent with hc send. Pass: the first turn answers the task.
# Run from a login shell: bash scripts/probes/p5-launch-task.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P5: launch-argument task survives the startup dialog"; exit 0 ;; esac
probe_init p5 "${1:-}"

REPO=$WORK/repo; new_repo "$REPO" || { say "RESULT p5: precondition-failed (repository)"; exit 2; }
# The answer (42-P5-DONE) is not in the prompt text, so seeing it means the agent answered.
TASK="Reply with the sum of 40 and 2 followed directly by -P5-DONE, and nothing else."
T=p5-worker
probe_dry_stop   # PROBE_DRY_RUN=1 ends here, through the exit handler
launch "$T" "$REPO" -- --permission-mode default "$TASK" || { record launch FAIL "hc run failed"; finish; }

i=0; seen=
while [ "$i" -lt 150 ]; do
  has_dialog "$T" && answer_dialog "$T"
  if hc read "$T" 2>/dev/null | grep -q '42-P5-DONE'; then seen=1; break; fi
  i=$((i + 3)); sleep 3
done

if [ "$DIALOGS" -eq 0 ]; then
  record task-after-dialog NEEDS-HUMAN "no trust dialog appeared (folder already trusted?), so the case was not exercised; answered=${seen:-no}"
elif [ -n "$seen" ]; then
  record task-after-dialog PASS "dialog answered $DIALOGS time(s); first turn answered the launch task with no hc send (${i}s)"
else
  record task-after-dialog FAIL "dialog answered $DIALOGS time(s); no answer to the launch task in 150s (task lost?)"
  screen_dump "$T" "after 150s"
fi
finish
