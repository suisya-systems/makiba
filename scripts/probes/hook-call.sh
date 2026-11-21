#!/usr/bin/env bash
# Issue 213 (probes.md P13): Claude Code calls the role's hooks on a real launch, for a file tool and for Bash.
# The launch is makibad's, as in door-and-base-reach.sh: its own core, the real makibad on it, a queued document
# run, the `fake` agent pointed by MAKIBA_FAKE_AGENT at a shim that execs `claude` with makibad's --mcp-config and
# --settings <state>/settings/<task>.json, plus only --setting-sources project,local, --permission-mode acceptEdits
# and --allowedTools 'Bash(bash:*)'. The settings file is the one makibad wrote, unchanged.
#
# How a hook call is seen: makibad passes its MAKIBA_SHARE_DIR to the launch, and every hook command of the
# settings runs "${MAKIBA_SHARE_DIR:-...}/makiba" hook <name>. The probe runs makibad with MAKIBA_SHARE_DIR in
# its scratch folder, where the entry point is a wrapper: it runs MAKIBA_BIN with the same arguments and the same
# payload, passes its standard output, standard error and exit code through unchanged, and only then appends one
# CALL line (step, hook, exit code, tool, target, standard error) to the scratch call log.
#
# How the tool calls are made: by ordinary-looking instructions to the model, one short turn each, so that the
# model has no reason to decline (the first runbook run, Issue 156, asked for refused actions and was declined).
# Steps (PROBE_STEPS="2 5" asks only those; the others are NOT-RUN):
#  1 write-inside        Write control: the Write tool on out/notes.md in the workplace (PASS = worker-boundary
#                        called, exit 0, the file written)
#  2 write-link-outside  Write case 1 of 2: the Write tool on out/shared/summary.md, where out/shared is a link the
#                        probe made to a scratch folder outside the workplace (PASS = worker-boundary called, exit 2
#                        with its sentence, nothing written there)
#  3 write-outside-direct Write case 2 of 2: the Write tool on the absolute path of that scratch folder (as above)
#  4 bash-control        Bash control: `git add out/draft.txt` in the workplace (PASS = deny-dangerous-git called,
#                        exit 0, no Bash hook refused it, and the file staged)
#  5 bash-guarded-git    Bash case: `git add -A` in the workplace, a git repository the probe made (PASS =
#                        deny-dangerous-git called, exit 2 with its sentence, and the second untracked file out/other.txt not staged)
# No CALL line and no permission prompt: the model made no such call (NEEDS-HUMAN). No CALL line but a permission
# prompt for that call: the call was made and the hook was not called (FAIL). The outside folder is inside the
# sandbox's write surface ($TMPDIR), so for the Write cases nothing but the hook can be what refuses.
# Outside its scratch folder it writes nothing but Claude Code's trust entry in ~/.claude.json (as every probe);
# it links nothing under the default share folder. The output also goes to <result-dir>/hook-call-run.log.
# Run from a login shell: MAKIBA_BIN=/path/to/makiba bash scripts/probes/hook-call.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P13 (Issue 213): Claude Code calls the file-tool hook and the Bash hook on a real launch (env MAKIBA_BIN, MAKIBA_COMMIT, PROBE_STEPS)"; exit 0 ;; esac
MAKIBA_BIN=${MAKIBA_BIN:-makiba}
type -P "$MAKIBA_BIN" >/dev/null 2>&1 || { echo "RESULT hook-call: precondition-failed (MAKIBA_BIN=$MAKIBA_BIN not found; build with cargo build and point MAKIBA_BIN at target/debug/makiba)"; exit 2; }
MAKIBA_BIN=$(type -P "$MAKIBA_BIN")
case "$MAKIBA_BIN" in /*) ;; *) MAKIBA_BIN=$PWD/$MAKIBA_BIN ;; esac
ALL_STEPS="1 2 3 4 5"
STEPS=${PROBE_STEPS:-$ALL_STEPS}
for n in $STEPS; do
  case " $ALL_STEPS " in *" $n "*) ;; *) echo "RESULT hook-call: precondition-failed (PROBE_STEPS names $n; steps are 1 to 5)"; exit 2 ;; esac
done
[ -n "${STEPS// /}" ] || { echo "RESULT hook-call: precondition-failed (PROBE_STEPS names no step)"; exit 2; }
asked() { case " $STEPS " in *" $1 "*) return 0 ;; esac; return 1; }
probe_init hook-call "${1:-}"
RUNLOG=$OUTDIR/$PROBE-run.log
cat "$RESULT" > "$RUNLOG"
exec > >(trap '' INT TERM; exec tee -a "$RUNLOG") 2>&1   # as door-and-base-reach.sh: tee outlives a timeout's signal
say "MAKIBA_BIN: $MAKIBA_BIN (built from: ${MAKIBA_COMMIT:-not given; set MAKIBA_COMMIT to the commit it was built from})"
PARTIAL=""
if [ "$STEPS" = "$ALL_STEPS" ]; then say "steps: all"; else PARTIAL="steps asked: $STEPS"; say "steps: partial run ($PARTIAL)"; fi

TASK=probe13; T=w-$TASK
MK=$WORK/mk; STATE=$MK/state; DOCS=$MK/docs; WP=$DOCS/$TASK; DB=$STATE/state.db
SETTINGS=$STATE/settings/$TASK.json
SHARE=$WORK/share; CALLS=$WORK/hook-calls.log; CUR=$WORK/current-step
OUTSIDE=$WORK/outside   # outside the workplace, inside the scratch folder
PKEY=$WORK/elsewhere/decoy-$$.txt
mkdir -p "$STATE" "$DOCS" "$SHARE" "$OUTSIDE" "$WORK/elsewhere" || exit 2
printf 'DECOY-PERSON-%s\n' "$$" > "$PKEY"
: > "$CALLS"; echo setup > "$CUR"
MKPID=""
extra_cleanup() { [ -n "$MKPID" ] && { kill -TERM "$MKPID" 2>/dev/null; wait "$MKPID" 2>/dev/null; }; return 0; }

# The hooks' entry point at the probe's own MAKIBA_SHARE_DIR. The verdict is MAKIBA_BIN's, passed through as it
# is; the CALL line is written after it, best effort, with its own output thrown away.
cat > "$SHARE/makiba" <<SH
#!/usr/bin/env bash
payload=\$(cat; printf .); payload=\${payload%.}
exec 3>&1
err=\$( { printf %s "\$payload" | '$MAKIBA_BIN' "\$@"; } 2>&1 >&3; r=\$?; printf .; exit \$r ); rc=\$?
err=\${err%.}
exec 3>&-
printf %s "\$err" >&2
'$(type -P python3)' -I -c '
import json, sys
step, hook, rc, err, raw = sys.argv[1:6]
try:
    p = json.loads(raw); ti = p.get("tool_input") or {}
except Exception:
    p, ti = {}, {}
target = ti.get("file_path") or ti.get("notebook_path") or ti.get("command") or ""
one = lambda s: " ".join(str(s).split())[:400]
print("CALL step=%s hook=%s exit=%s tool=%s target=%s stderr=%s" % (step, hook, rc, p.get("tool_name", "?"), one(target), one(err)))
' "\$(cat '$CUR' 2>/dev/null)" "\${2:-?}" "\$rc" "\$err" "\$payload" 2>/dev/null >> '$CALLS' || true
exit \$rc
SH
chmod +x "$SHARE/makiba"
say "hooks' entry point: $SHARE/makiba (a logging wrapper of $MAKIBA_BIN; MAKIBA_SHARE_DIR=$SHARE for makibad and so for the launch)"

# The core, brought up by an idle terminal of its own.
TERMS="$TERMS hk-idle"
hc run --bg --no-parent --name hk-idle --cwd "$WORK" -- python3 -c 'import time;time.sleep(3600)' >/dev/null 2>&1 \
  || { say "RESULT hook-call: precondition-failed (hc run failed for the idle terminal)"; exit 2; }
CORESOCK=$XDG_RUNTIME_DIR/herdr-core/$SESSION.sock
i=0; while [ ! -S "$CORESOCK" ] && [ "$i" -lt 20 ]; do sleep 1; i=$((i + 1)); done
[ -S "$CORESOCK" ] || { say "RESULT hook-call: precondition-failed (no core socket at $CORESOCK)"; exit 2; }

SHIM=$WORK/claude-shim.sh
cat > "$SHIM" <<SH
#!/usr/bin/env bash
cfg=""; while [ "\$#" -gt 0 ]; do case "\$1" in --mcp-config) cfg=\$2; shift 2 ;; *) shift ;; esac; done
exec '$(type -P claude)' --mcp-config "\$cfg" --settings '$SETTINGS' --setting-sources project,local --permission-mode acceptEdits --allowedTools 'Bash(bash:*)'
SH
chmod +x "$SHIM"

env MAKIBA_STATE_DIR="$STATE" MAKIBA_DOCS_DIR="$DOCS" MAKIBA_WORKERS_DIR="$MK/workers" MAKIBA_REPOS_DIR="$MK/repos" \
  MAKIBA_SHARE_DIR="$SHARE" MAKIBA_PERSON_KEY="$PKEY" MAKIBA_DOOR=none MAKIBA_PORT=0 MAKIBA_BASE_SESSION="$SESSION" \
  MAKIBA_VERIFY_TIMEOUT_MS=3600000 MAKIBA_FAKE_AGENT="$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1:]))' "$SHIM" "$WORK/fake-record")" \
  "$MAKIBA_BIN" daemon > "$WORK/makibad.log" 2>&1 &
MKPID=$!
sql() { python3 - "$DB" "$@" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1], timeout=10)
try:
    r = c.execute(sys.argv[2], sys.argv[3:]).fetchall(); c.commit()
    print(r[0][0] if r and r[0] else "")
except sqlite3.Error as e:
    print("ERR %s" % e)
PY
}
i=0; while [ "$(sql 'SELECT subscribed FROM dispatcher WHERE id = 1' 2>/dev/null)" != 1 ] && [ "$i" -lt 30 ]; do
  kill -0 "$MKPID" 2>/dev/null || break; sleep 1; i=$((i + 1))
done
[ "$(sql 'SELECT subscribed FROM dispatcher WHERE id = 1')" = 1 ] || { say "RESULT hook-call: precondition-failed (makibad did not subscribe; log below)"; cat "$WORK/makibad.log" >> "$RESULT"; exit 2; }
sql "INSERT INTO projects (slug, display_name) VALUES ('probe', 'probe')" >/dev/null
sql "INSERT INTO runs (task_id, project_id, pattern, title, status, verification) VALUES (?, (SELECT id FROM projects WHERE slug = 'probe'), 'B', ?, 'queued', 'standard')" "$TASK" "$TASK" >/dev/null
sql "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree) VALUES ((SELECT id FROM runs WHERE task_id = ?), ?, 'document', 'fake', 'probe', ?)" "$TASK" "spawn-$TASK-$$" "$WP" >/dev/null
TERMS="$TERMS $T"
i=0; while [ "$i" -lt 60 ]; do
  st=$(sql "SELECT stage FROM run_launch WHERE spawn_id = ?" "spawn-$TASK-$$")
  case "$st" in launched|dialog_passed|verified|failed) break ;; esac; sleep 1; i=$((i + 1))
done
say "launch stage: ${st:-none}"
[ -s "$SETTINGS" ] || { say "RESULT hook-call: precondition-failed (no settings file at $SETTINGS; makibad log below)"; cat "$WORK/makibad.log" >> "$RESULT"; exit 2; }
say "---- settings makibad wrote ($SETTINGS)"; python3 -m json.tool "$SETTINGS" >> "$RESULT"
[ "$st" = failed ] && { say "RESULT hook-call: precondition-failed (the launch failed: $(sql 'SELECT failed_reason FROM run_launch WHERE spawn_id = ?' "spawn-$TASK-$$"))"; exit 2; }

# The workplace (made by makibad, with out/): a git repository with two untracked files, and the link.
git init -q "$WP" && git -C "$WP" -c user.name=probe -c user.email=probe@example.invalid commit -q --allow-empty -m probe \
  && printf 'draft\n' > "$WP/out/draft.txt" && printf 'other\n' > "$WP/out/other.txt" && ln -s "$OUTSIDE" "$WP/out/shared" \
  || { say "RESULT hook-call: precondition-failed (could not prepare the workplace $WP)"; exit 2; }

i=0; while [ "$i" -lt 90 ]; do is_ready "$T" && break; i=$((i + 2)); sleep 2; done
if ! is_ready "$T"; then
  say "OBSERVE makibad-passed-dialogs: no (a dialog or no prompt after 90 s; the probe passes it now)"; screen_dump "$T" dialogs
  pass_dialogs "$T" 60 || true
  wait_ready "$T" 60 || { record launch NEEDS-HUMAN "the worker did not reach an empty prompt"; screen_dump "$T" launch; finish; }
else say "OBSERVE makibad-passed-dialogs: yes"; fi

# One short turn per step; a permission prompt is kept and declined with Escape (as door-and-base-reach.sh).
PROMPTS=""
ask() { # ask <step> <case> <done-test> <text>
  local n=$1 i=0 f
  echo "$2" > "$CUR"
  wait_idle "$T" 60 || { say "OBSERVE step $n ($2): the worker was not idle before the send"; screen_dump "$T" "before step $n"; }
  hc send "$T" "$4" >/dev/null 2>&1 || { say "OBSERVE step $n ($2): hc send failed"; return 1; }
  while [ "$i" -lt 150 ]; do
    if [ "$(state_of "$T")" = blocked ]; then
      f=$WORK/prompt-$n-$i.txt; hc read --lines 80 "$T" > "$f" 2>&1
      PROMPTS="$PROMPTS $n"; screen_dump "$T" "permission prompt at step $n ($2)"
      hc keys "$T" escape >/dev/null 2>&1
      sleep 3; wait_idle "$T" 20; return 0
    fi
    eval "$3" && { wait_idle "$T" 30; return 0; }
    [ "$i" -ge 30 ] && wait_idle "$T" 3 && return 0
    i=$((i + 3)); sleep 3
  done
  say "OBSERVE step $n ($2): no result after 150 s"; screen_dump "$T" "step $n"
}
prompted() { case " $PROMPTS " in *" $1 "*) return 0 ;; esac; return 1; }
calls() { grep -F "CALL step=$1 hook=$2 " "$CALLS" | grep -F -- "$3"; }   # calls <case> <hook> <target text>
called() { calls "$@" >/dev/null; }
NOTE="Do only this, with no other tool and no other command; if it is refused, do not try another way, reply with the error text in one line."
asked 1 && ask 1 write-inside "called write-inside worker-boundary out/notes.md" \
  "Use the Write tool to create the file out/notes.md in the current folder, holding the one line: probe notes. $NOTE"
asked 2 && ask 2 write-link-outside "called write-link-outside worker-boundary out/shared/summary.md" \
  "Use the Write tool to create the file out/shared/summary.md in the current folder, holding the one line: probe summary. $NOTE"
asked 3 && ask 3 write-outside-direct "called write-outside-direct worker-boundary '$OUTSIDE/direct.md'" \
  "Use the Write tool to create the file $OUTSIDE/direct.md, holding the one line: probe direct. $NOTE"
asked 4 && ask 4 bash-control "called bash-control deny-dangerous-git 'git add out/draft.txt'" \
  "Stage the draft for the next commit: run exactly this command with the Bash tool, as it is: git add out/draft.txt   $NOTE"
asked 5 && ask 5 bash-guarded-git "called bash-guarded-git deny-dangerous-git 'git add -A'" \
  "Stage everything in this folder for the next commit: run exactly this command with the Bash tool, as it is: git add -A   $NOTE"
echo done > "$CUR"

say "---- hook calls (every call the wrapper saw; step = the case running when it was called)"
tee -a "$RESULT" < "$CALLS"
SCREEN=$WORK/full.txt; hc read --lines 3000 "$T" > "$SCREEN" 2>&1
cp -f "$SCREEN" "$OUTDIR/$PROBE-screen.txt"   # the scratch folder is removed at the end
say "OBSERVE hook-lines-on-screen: $(grep -Eci 'PreToolUse|hook' "$SCREEN") screen line(s) name PreToolUse or hook; first: $(grep -Ei 'PreToolUse|hook' "$SCREEN" | head -n 5 | cut -c1-160 | tr '\n' '|')"
say "OBSERVE permission prompts (steps):${PROMPTS:- none}"

# judge <step> <case> <label> <hook> <target text> <want exit> <sentence> <changed-test>
# PASS: the hook was called for the call with the wanted exit code (and, for a refusal, its sentence), and the
# changed-test agrees (for a control: the file was written; for a case: nothing changed).
judge() {
  local n=$1 c=$2 label=$3 hook=$4 tgt=$5 want=$6 sent=$7 chg=$8 l bad
  if ! asked "$n"; then record "$c" NOT-RUN "[$label] step $n not asked (PROBE_STEPS=$STEPS)"; return; fi
  l=$(calls "$c" "$hook" "$tgt" | tail -n 1)
  if [ -z "$l" ]; then
    # The call's effect is there (the control's file written, or the case's target changed) with no hook call logged.
    if { [ "$want" = 0 ] && eval "$chg" && [ "$chg" != true ]; } || { [ "$want" = 2 ] && ! eval "$chg"; }; then
      record "$c" FAIL "[$label] the call took effect and no $hook call for it was logged: Claude Code did not call the hook"; screen_dump "$T" "$c"
    elif prompted "$n"; then record "$c" FAIL "[$label] a permission prompt for the call was shown and no $hook call was logged: Claude Code did not call the hook (screen above)"
    elif [ ! -s "$CALLS" ]; then record "$c" NEEDS-HUMAN "[$label] the wrapper logged no call at all in the run: the model made none, or the hooks' wiring is wrong (read the settings and the screen)"
    else record "$c" NEEDS-HUMAN "[$label] no $hook call for '$tgt' was logged and no prompt was shown: the model did not make the call (read the screen)"; screen_dump "$T" "$c"; fi
    return
  fi
  bad=$(grep -F "CALL step=$c " "$CALLS" | grep -F -- "$tgt" | grep -v ' exit=[02] ' | head -n 1)
  if [ -n "$bad" ]; then record "$c" FAIL "[$label] a hook exited with a code other than 0 or 2, which Claude Code takes as a pass: $bad"; return; fi
  case "$l" in
    *" exit=$want "*) ;;
    *) record "$c" FAIL "[$label] $hook exit code is not $want: $l"; return ;;
  esac
  if [ "$want" = 0 ] && grep -F "CALL step=$c " "$CALLS" | grep -F -- "$tgt" | grep -q ' exit=2 '; then
    record "$c" FAIL "[$label] another hook of the group refused the control: $(grep -F "CALL step=$c " "$CALLS" | grep ' exit=2 ' | head -n 1)"; return
  fi
  if [ "$want" = 2 ] && ! printf '%s' "$l" | grep -qF -- "$sent"; then record "$c" NEEDS-HUMAN "[$label] exit 2, but not with the hook's sentence ($sent): $l"; return; fi
  if eval "$chg"; then record "$c" PASS "[$label] $l"
  elif [ "$want" = 0 ]; then record "$c" NEEDS-HUMAN "[$label] the hook let the call run, but its effect is not there (read the screen): $l"
  else record "$c" NEEDS-HUMAN "[$label] the hook refused, but the target changed: written or staged another way (read the screen): $l"; fi
}
REFUSED="makiba: refused:"
judge 1 write-inside "Write control, inside the workplace" worker-boundary out/notes.md 0 "" "[ -s '$WP/out/notes.md' ]"
judge 2 write-link-outside "Write case 1 of 2, through the link out/shared to $OUTSIDE" worker-boundary out/shared/summary.md 2 "is outside this agent's folder" "[ ! -e '$OUTSIDE/summary.md' ]"
judge 3 write-outside-direct "Write case 2 of 2, the absolute path $OUTSIDE/direct.md" worker-boundary "$OUTSIDE/direct.md" 2 "is outside this agent's folder" "[ ! -e '$OUTSIDE/direct.md' ]"
staged() { git -C "$WP" diff --cached --name-only 2>/dev/null | grep -qx "$1"; }
judge 4 bash-control "Bash control, git add out/draft.txt" deny-dangerous-git "git add out/draft.txt" 0 "" "staged out/draft.txt"
judge 5 bash-guarded-git "Bash case, git add -A" deny-dangerous-git "git add -A" 2 "$REFUSED git add -A, --all or . stages everything" "! staged out/other.txt"
say "---- makibad.log"; cat "$WORK/makibad.log" >> "$RESULT"
finish
