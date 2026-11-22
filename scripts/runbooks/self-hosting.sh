#!/usr/bin/env bash
# The stages of docs/runbooks/self-hosting.md (Issue 50, M7-1), one line each, typed by the person
# in a login shell on the host (never inside an agent: makiba up refuses that):
#
#   bash scripts/runbooks/self-hosting.sh check       read-only: the preconditions, nothing is changed
#   bash scripts/runbooks/self-hosting.sh rehearse    the whole run in a scratch place; no real place is written
#   bash scripts/runbooks/self-hosting.sh first-day   the four checks before the first range, in a scratch place
#   bash scripts/runbooks/self-hosting.sh register    REAL: makiba registered in the organization, ledger placed
#   bash scripts/runbooks/self-hosting.sh triage      REAL: one small task dispatched and closed, the scan read
#   bash scripts/runbooks/self-hosting.sh switch      REAL: restart the organization on origin/main, with the secretary; starts the count
#   bash scripts/runbooks/self-hosting.sh cleanup     removes the scratch places this script made; safe to run twice
#
# Each stage takes --dry-run (prints the commands, runs none, writes no file) and --yes (the real
# stages ask once before they change anything). `register` also takes --replace-ledger;
# `switch` also takes --handover <file> (the owner's private handover for the secretary) and
# --issues 1,2,3 (the Issues of the first range; without it the secretary reads them from the handover).
# Each stage writes a results file for the secretary, <repo>/.runbook-results/self-hosting-<stage>-<UTC>.txt
# (RUNBOOK_RESULTS_DIR moves it). Nothing is deleted except what this script created.
# Exit 0 done, 1 a check failed, 2 usage, 3 refused (a precondition is missing; the fix is printed).
set -eu
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
# shellcheck source=lib.sh
. "$HERE/lib.sh"

SLUG=makiba
GH_REPO=suisya-systems/makiba
# The crates hosts a cargo build of makiba needs, so that a later `code` run of makiba can build. Only the
# host run proves the list is enough: a host outside it is refused with no prompt (docs/design/lines.md L-K7).
HOSTS=${MAKIBA_RUNBOOK_HOSTS:-index.crates.io,static.crates.io}
MK_GIVEN=${MK:+1}   # a makiba binary the person named (MK=...) is used as it is; the default is built from this commit
MK=${MK:-$REPO/target/debug/makiba}
ASSUME_YES=0
REPLACE_LEDGER=0
STAGE=""
SCRATCH_PREFIX=makiba-rehearsal-
ORIG_TMP=${TMPDIR:-/tmp}   # scratch_env moves TMPDIR; the scratch folders are named from this one
BRIEF=""                   # the temporary brief of the triage task, removed at exit
SCRATCH_DRY=0           # 1 in a dry run of a scratch stage: paths are printed in their scratch form
SCRATCH_RUN=""             # the scratch folder of this rehearsal, stopped and (when clean) removed at exit

usage() { sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

SWITCH_HANDOVER=""
SWITCH_ISSUES=""
while [ $# -gt 0 ]; do
  a=$1; shift
  case "$a" in
    --dry-run) DRY=1 ;;
    --yes) ASSUME_YES=1 ;;
    --replace-ledger) REPLACE_LEDGER=1 ;;
    --handover) [ $# -gt 0 ] || { echo "--handover needs a file" >&2; exit 2; }; SWITCH_HANDOVER=$1; shift ;;
    --handover=*) SWITCH_HANDOVER=${a#--handover=} ;;
    --issues) [ $# -gt 0 ] || { echo "--issues needs a list like 169,270" >&2; exit 2; }; SWITCH_ISSUES=$1; shift ;;
    --issues=*) SWITCH_ISSUES=${a#--issues=} ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option $a" >&2; usage >&2; exit 2 ;;
    *) if [ -n "$STAGE" ]; then usage >&2; exit 2; fi; STAGE=$a ;;
  esac
done
case "$STAGE" in check | rehearse | first-day | register | triage | switch | cleanup) ;; *) usage >&2; exit 2 ;; esac
if [ -n "$SWITCH_HANDOVER$SWITCH_ISSUES" ] && [ "$STAGE" != switch ]; then echo "--handover and --issues belong to the switch stage" >&2; exit 2; fi
if [ -n "$SWITCH_ISSUES" ] && ! printf '%s' "$SWITCH_ISSUES" | grep -Eq '^[0-9]+(,[0-9]+)*$'; then echo "--issues takes Issue numbers separated by commas, like 169,270" >&2; exit 2; fi
STAGE_NAME=$STAGE

open_results() {
  if [ "$DRY" = 1 ]; then return 0; fi
  local dir=${RUNBOOK_RESULTS_DIR:-$REPO/.runbook-results}
  mkdir -p "$dir"
  RESULT="$dir/self-hosting-$STAGE-$(date -u +%Y%m%dT%H%M%SZ).txt"
  : > "$RESULT"
}

# ---- preconditions (runbook section 5) ----

# The variables `makiba up` refuses (src/up.rs): any CLAUDE*, and the base's own session variables.
# HC_BIN, which names the hc program, is not one of them.
not_in_agent() { ! env | grep -qE '^(CLAUDE|HC_SESSION=|HC_SOCKET=|HC_SCOPE=|HC_TERMINAL_ID=|HC_TERMINAL_SOCKET=)'; }
git_new_enough() { local v; v=$(git --version | awk '{print $3}'); [ "$(printf '%s\n2.38\n' "$v" | sort -V | head -n 1)" = 2.38 ]; }
gh_login() { env -u GH_TOKEN -u GITHUB_TOKEN -u GH_ENTERPRISE_TOKEN -u GITHUB_ENTERPRISE_TOKEN gh auth status -h github.com; }
py_sqlite() { python3 -I -c 'import sqlite3; assert sqlite3.connect(":memory:").execute("SELECT json_extract(?, ?)", ("{\"a\": 1}", "$.a")).fetchone()[0] == 1'; }
incidents_ok() {
  [ "$(grep -v '^#' "$REPO/docs/design/incidents-pending.txt" | grep -c . || true)" = 0 ] \
    && (cd "$REPO" && python3 tools/check_incidents.py)
}

# must <label> <what is missing> <how to fix> <command...>: prints what it checks; refuses when it fails.
must() {
  local label=$1 what=$2 fix=$3; shift 3
  say "+ check: $label   ($*)"
  if [ "$DRY" = 1 ]; then return 0; fi
  "$@" >/dev/null 2>&1 || refuse "$what" "$fix"
}

# The clause Issue 50 names (its "Goal clause" heading, issues/M7-1.md), e.g. G9.
issue_clause() { sed -n '/^## Goal clause/,$p' "$REPO/issues/M7-1.md" | grep -Eo '^- G[0-9]+' | head -n 1 | sed 's/^- //'; }

# ledger_clauses <file> <label>: how many clauses it has, and whether it holds the clause Issue 50 names.
ledger_clauses() {
  local f=$1 label=$2 want n head
  want=$(issue_clause)
  [ -n "$want" ] || refuse "issues/M7-1.md names no goal clause" "check the Issue's '## Goal clause' list"
  n=$(grep -c '^## G[0-9]' "$f" || true)
  head=$(grep -m1 "^## $want " "$f" || true)
  if [ -n "$head" ]; then
    check "ledger-$label" PASS "$n clauses; Issue 50 names $want and the ledger has it: ${head#\#\# }"
  else
    check "ledger-$label" FAIL "$n clauses; Issue 50 names $want and the ledger has no such clause"
    refuse "the $label ledger has no clause $want, which Issue 50 names (the scan would show no candidate for it)" \
      "land the ledger change (pull request 260), pull main, then place it again: self-hosting.sh register --replace-ledger"
  fi
}

# The real organization's own state: running or not, and the project's folder if there is one.
real_places() {
  local dir=${MAKIBA_REPOS_DIR:-$HOME/makiba-repos}/$SLUG
  say "+ $MK status        (is an organization already running on the default places)"
  say "+ git -C $dir remote get-url origin; git -C $dir status --short --branch --untracked-files=no"
  if [ "$DRY" = 1 ]; then return 0; fi
  if "$MK" status >/dev/null 2>&1; then check organization INFO "makibad already runs here; it is not started again"
  else check organization INFO "makibad does not run here; register starts it"; fi
  if [ -e "$dir" ]; then
    local url branch
    url=$(git -C "$dir" remote get-url origin 2>/dev/null || true)
    case "$url" in
      *"$GH_REPO" | *"$GH_REPO.git") ;;
      *) refuse "$dir exists and its origin is '$url', not $GH_REPO (makibad would stop with a failed command)" "move that folder away yourself, or set MAKIBA_REPOS_DIR; this script does not touch it" ;;
    esac
    branch=$(git -C "$dir" rev-parse --abbrev-ref HEAD)
    [ "$branch" = main ] || refuse "$dir is on '$branch', not on its default branch" "git -C $dir switch main"
    [ -z "$(git -C "$dir" status --porcelain --untracked-files=no)" ] || refuse "$dir has uncommitted changes to tracked files" "commit or discard them in that folder"
    check clone-folder INFO "$dir exists, origin $GH_REPO, on main and clean; makibad fetches it"
  else
    check clone-folder INFO "$dir does not exist; makibad clones it"
  fi
}

preflight() { # preflight <real|scratch>
  local mode=$1
  say "== preconditions ($mode)"
  must "not inside an agent" "a CLAUDE* or HC_SESSION/HC_SOCKET/HC_SCOPE/HC_TERMINAL_* variable is set: this shell is inside an agent" "open a plain login shell on the host and run the line again" not_in_agent
  for c in git python3 gh cargo "${HC_BIN:-hc}" claude; do
    must "$c on PATH" "$c is not on PATH" "install it, or put it on PATH" have "$c"
  done
  must "git 2.38 or newer" "git is older than 2.38 (git merge-tree --write-tree)" "install git 2.38 or newer" git_new_enough
  must "python3 sqlite3 with json_extract" "python3 has no sqlite3 module with the JSON functions" "use a python3 whose sqlite3 has json_extract" py_sqlite
  must "stored login for github.com" "the GitHub CLI has no stored login for github.com (a login held in GH_TOKEN does not reach makibad)" "run: gh auth login -h github.com" gh_login
  must "pending list empty, check_incidents.py passes" "the pending list is not empty, or tools/check_incidents.py fails (Issue 50 definition of done 3)" "fix the incident table first: python3 tools/check_incidents.py" incidents_ok
  if [ "$DRY" = 1 ]; then
    plan "print the versions of hc, claude, gh and git, and the commit"
    plan "count the clauses of goals/suisya-systems/makiba.md and check it has the clause issues/M7-1.md names"
  else
    check versions INFO "hc $("${HC_BIN:-hc}" --version 2>&1 | head -n 1); claude $(claude --version 2>&1 | head -n 1); $(gh --version | head -n 1); $(git --version)"
    check commit INFO "$COMMIT"
    ledger_clauses "$REPO/goals/suisya-systems/makiba.md" repository
  fi
  if [ -z "$MK_GIVEN" ]; then
    say "  building this commit (an older target/debug/makiba would be run otherwise)"
    run cargo build --manifest-path "$REPO/Cargo.toml" || {
      check binary FAIL "cargo build of $COMMIT failed (its output is above)"
      refuse "cargo build failed" "fix the build, then run the line again"
    }
  fi
  if [ "$DRY" != 1 ] && [ ! -x "$MK" ]; then refuse "$MK is not an executable" "build it: cargo build, or name another with MK=<path>"; fi
  if [ "$mode" = real ]; then
    for v in MAKIBA_BASE MAKIBA_LAUNCH_AGENT MAKIBA_FAKE_AGENT; do
      if [ -n "${!v:-}" ]; then refuse "$v is set (a test seam)" "unset $v and run the line again"; fi
    done
    [ "${MAKIBA_DOOR:-}" != none ] || refuse "MAKIBA_DOOR=none is set (a test seam)" "unset MAKIBA_DOOR and run the line again"
    real_places
  fi
}

# ---- the stages ----

lines_changed() { # lines_changed <what this stage is about to change>: shown before the real stages go on
  say "This stage is about to change:"
  printf '%s\n' "$1" | while IFS= read -r l; do say "  - $l"; done
  say "Runbook section 1 lists what cannot be undone afterwards; read it first."
}

place_ledger() {
  local goals=${MAKIBA_GOALS_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/makiba/goals}
  # A dry run does not create the scratch place, so its paths would be the real ones: print the scratch form.
  if [ "$DRY" = 1 ] && [ "$SCRATCH_DRY" = 1 ]; then goals='<scratch>/config/makiba/goals'; fi
  local src=$REPO/goals/suisya-systems/makiba.md dst=$goals/suisya-systems/makiba.md
  say "== place the goal ledger: $dst"
  if [ "$DRY" = 1 ]; then
    plan "create $goals/suisya-systems if missing and copy the repository's ledger there when none is placed"
    plan "when one is placed and differs: keep it, unless --replace-ledger (then the old one is kept as makiba.md.before-<UTC>)"
    plan "count the placed ledger's clauses and check it has the clause Issue 50 names"
    return 0
  fi
  if [ ! -e "$dst" ]; then
    mkdir -p "$goals/suisya-systems"
    cp "$src" "$dst"
    check ledger-placed PASS "copied the repository's ledger to $dst"
  elif cmp -s "$src" "$dst"; then
    check ledger-placed INFO "$dst matches the repository's ledger"
  elif [ "$REPLACE_LEDGER" = 1 ]; then
    local keep
    keep="$dst.before-$(date -u +%Y%m%dT%H%M%SZ)"
    cp "$dst" "$keep"
    cp "$src" "$dst"
    check ledger-placed PASS "replaced; the old copy is $keep"
  else
    check ledger-placed INFO "$dst differs from the repository's ledger and is kept (diff $src $dst; --replace-ledger replaces it)"
  fi
  ledger_clauses "$dst" placed
}

stage_register() {
  say "== register $SLUG in the organization"
  local scratch=${1:-0} up=()
  if [ "$scratch" = 1 ]; then up=(--no-secretary); elif [ "${NO_SECRETARY:-0}" = 1 ]; then up=(--no-secretary); fi
  say "network hosts for the project's workers: $HOSTS (MAKIBA_RUNBOOK_HOSTS changes the list)"

  if [ "$DRY" = 1 ]; then
    plan "run: $MK status; start the organization with: $MK up ${up[*]:-} only when makibad does not run"
    plan "read allowed_repo_owners; set it to the old list plus suisya-systems only when suisya-systems is not in it"
  else
    if "$MK" status >/dev/null 2>&1; then check up INFO "makibad already runs: not started again"
    else
      say "+ $MK up ${up[*]:-}"
      "$MK" up ${up[@]+"${up[@]}"}
      check up PASS "organization started"
    fi
    local cur new
    cur=$("$MK" config get allowed_repo_owners | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get("value") or "")') \
      || refuse "cannot read allowed_repo_owners" "run: $MK config get allowed_repo_owners"
    case ",$cur," in
      *,suisya-systems,*) check allowed_repo_owners INFO "already lists suisya-systems ($cur)" ;;
      *)
        new=${cur:+$cur,}suisya-systems
        say "+ $MK config set allowed_repo_owners $new"
        "$MK" config set allowed_repo_owners "$new"
        check allowed_repo_owners PASS "now $new"
        ;;
    esac
  fi

  # The project. Skipped when status already shows it with the same hosts, so a second run adds no event.
  local shown=""
  if [ "$DRY" != 1 ]; then shown=$("$MK" status 2>/dev/null | grep "^  $SLUG: $GH_REPO " || true); fi
  if [ "$DRY" != 1 ] && [ -n "$shown" ] && printf '%s' "$shown" | grep -qF "network hosts: ${HOSTS//,/, })"; then
    check project INFO "already registered with these hosts: $shown"
  else
    run "$MK" project "$SLUG" "$GH_REPO" --hosts "$HOSTS"
    if [ "$DRY" = 1 ]; then
      plan "wait until the newest 'project' command is done (the clone can take minutes; up to 30 minutes)"
    else
      project_done() { [ "$(sq "SELECT status FROM commands WHERE verb = 'project' ORDER BY id DESC LIMIT 1")" != accepted ]; }
      wait_for "${WAIT_CLONE:-1800}" "the project command to finish" project_done || refuse "the project command did not finish in time" "read makibad.log in the state folder; run the line again"
      local row
      row=$(sq "SELECT status, coalesce(error_kind, ''), coalesce(result_json, '') FROM commands WHERE verb = 'project' ORDER BY id DESC LIMIT 1")
      case "$row" in
        done\|*) check project PASS "command $row" ;;
        *) check project FAIL "command $row"; refuse "the project command failed: $row" "runbook section 6, the table 'If it differs'; then run the line again" ;;
      esac
    fi
  fi

  say "+ $MK status        (DoD 1: the project is listed)"
  if [ "$DRY" != 1 ]; then
    "$MK" status 2>&1 | sed 's/^/  | /' | tee -a "$RESULT"
    if "$MK" status 2>/dev/null | grep -q "^  $SLUG: $GH_REPO "; then check status-lists-project PASS "makiba status shows $SLUG"
    else check status-lists-project FAIL "makiba status does not list $SLUG"; fi
  fi
  place_ledger
}

# Verbs of pull requests that may not have landed (257: range, 261: review): read when the build has
# them, skipped with a printed line when it does not. Nothing here approves or reviews anything.
optional_verbs() {
  say "== optional verbs (read only)"
  say "+ $MK --help        (is a 'range' verb (pull request 257) or a 'review' verb (pull request 261) in this build)"
  if [ "$DRY" = 1 ]; then plan "print each verb's first help line when present, or 'skipped' when not"; return 0; fi
  local v pr
  for v in range review; do
    pr=257; [ "$v" = review ] && pr=261
    if "$MK" --help 2>&1 | grep -Eq "^  $v( |$)"; then
      check "verb-$v" INFO "present: $("$MK" "$v" --help 2>&1 | head -n 1) (the status read above shows what it adds; this runbook uses neither)"
    else
      check "verb-$v" INFO "not in this build: skipped (pull request $pr has not landed here)"
    fi
  done
}

scan_report() {
  local row st repos n refs diag err
  row=$(sq "SELECT json_extract(payload_json, '\$.status'), json_extract(payload_json, '\$.repos'), json_extract(payload_json, '\$.candidate_count'), coalesce(json_extract(payload_json, '\$.candidate_refs'), ''), coalesce(json_extract(payload_json, '\$.diagnostics'), ''), coalesce(json_extract(payload_json, '\$.error'), '') FROM events WHERE kind = 'work_discovery_scanned' ORDER BY id DESC LIMIT 1")
  IFS='|' read -r st repos n refs diag err <<EOF
$row
EOF
  check scan INFO "status=$st repos=$repos candidate_count=$n candidate_refs=$refs diagnostics=$diag error=$err"
  # candidate_count is the total over every scanned project: only the refs of makiba count here.
  local ref num k=0 mine=""
  for ref in $(printf '%s' "$refs" | tr ',' ' '); do
    case "$ref" in "$GH_REPO#"*) mine="$mine $ref" ;; esac
  done
  local mine_n; mine_n=$(printf '%s\n' $mine | grep -c . || true)
  if [ "$st" = candidates_found ] && [ "$mine_n" -gt 0 ] && printf '%s' "$repos" | grep -q "$GH_REPO" \
    && ! printf '%s' "$diag" | grep -qE "(unset|error):$GH_REPO"; then
    check dod-2-triage-returns-candidates PASS "$mine_n candidates of $GH_REPO (scan total $n)"
  else
    check dod-2-triage-returns-candidates FAIL "no candidate of $GH_REPO (scan total $n); see the scan line above and the table of runbook section 7"
  fi
  for ref in $mine; do
    [ "$k" -lt 2 ] || break
    num=${ref##*#}; k=$((k + 1))
    say "+ gh issue view $num --repo $GH_REPO --json state"
    local state; state=$(gh issue view "$num" --repo "$GH_REPO" --json state -q .state 2>&1 | head -n 1)
    if [ "$state" = OPEN ]; then check "candidate-$num-is-open" PASS "$ref is OPEN"; else check "candidate-$num-is-open" FAIL "$ref: $state"; fi
  done
  say "== the notice to the secretary"
  sq "SELECT id, body FROM messages WHERE sender = 'makibad' AND body LIKE 'Triage after a close:%' ORDER BY id DESC LIMIT 1" | tee -a "$RESULT" || true
  say "== makiba status (next work)"
  "$MK" status 2>&1 | sed 's/^/  | /' | tee -a "$RESULT" || true
  optional_verbs
}

stage_triage() {
  local scratch=${1:-0}
  TASK=${MAKIBA_RUNBOOK_TASK:-m71-triage-$(date -u +%y%m%d%H%M)}
  say "== triage scan: one small document task '$TASK', its close, then the scan"
  local docs=${MAKIBA_DOCS_DIR:-$HOME/makiba-docs}
  if [ "$DRY" = 1 ]; then
    plan "read the id and age of the newest work_discovery_scanned event (a scan under 30 minutes old delays the next one)"
    say "+ $MK dispatch --kind document --task-id $TASK --title 'Triage trigger (M7-1)' --brief-file <temporary brief>"
    plan "wait until run $TASK is 'review' and its report_done message is there (up to 15 minutes)"
    say "+ $MK close $TASK"
    plan "wait for a work_discovery_scanned event newer than the run_closed event (up to 40 minutes)"
    plan "print status, repos, candidate_count, candidate_refs, diagnostics; check two candidates are open Issues with gh; print the notice and makiba status"
    say "  its folder: $docs/$TASK (out/ stays after the close)"
    return 0
  fi
  [ "$("$MK" status >/dev/null 2>&1 && echo up)" = up ] || refuse "makibad does not run on this place" "run the register stage first (or rehearse for the scratch place)"
  [ -n "$(sq "SELECT 1 FROM projects WHERE slug = '$SLUG'")" ] || refuse "project $SLUG is not registered here" "run the register stage first"
  local before age
  before=$(sq "SELECT coalesce(max(id), 0) FROM events WHERE kind = 'work_discovery_scanned'")
  age=$(sq "SELECT coalesce((julianday('now') - julianday(max(occurred_at))) * 1440, -1) FROM events WHERE kind = 'work_discovery_scanned'")
  check last-scan INFO "newest scan event id $before, age ${age%%.*} minutes (-1: none yet); a scan under 30 minutes old makes the next wait for its end"

  local brief
  brief=$(mktemp "$ORIG_TMP/makiba-brief-XXXXXX")
  BRIEF=$brief
  printf 'Write the file out/note.md with the single line "triage trigger for M7-1". When it is written, call the tool report with state "done", task "%s" and one sentence that says what you wrote.\n' "$TASK" > "$brief"
  say "+ $MK dispatch --kind document --task-id $TASK --title 'Triage trigger (M7-1)' --brief-file $brief"
  "$MK" dispatch --kind document --task-id "$TASK" --title "Triage trigger (M7-1)" --brief-file "$brief"
  review_ready() { [ "$(sq "SELECT status FROM runs WHERE task_id = '$TASK'")" = review ] && [ -n "$(sq "SELECT 1 FROM messages WHERE kind = 'report_done' AND run_id = (SELECT id FROM runs WHERE task_id = '$TASK')")" ]; }
  if ! wait_for "${WAIT_REPORT:-900}" "the worker's done report" review_ready; then
    check dispatch FAIL "no done report in time"
    refuse "the worker did not report (a stop, or a dialog it cannot pass)" \
      "read its screen, then end it yourself with: $MK close $TASK --abandon   (and say why in the results)"
  fi
  check dispatch PASS "run $TASK is review with a done report"
  say "+ $MK close $TASK"
  "$MK" close "$TASK"
  check close PASS "closed $TASK"

  scanned() { [ -n "$(sq "SELECT 1 FROM events WHERE kind = 'work_discovery_scanned' AND id > (SELECT max(id) FROM events WHERE kind = 'run_closed')")" ]; }
  wait_for "${WAIT_SCAN:-2400}" "the scan after the close" scanned || refuse "no scan after the close" "$MK status shows whether makibad runs; the age check above says how long the interval can take"
  scan_report "$scratch"
}

# ---- the scratch place (rehearsal) ----

scratch_env() {
  if [ "$DRY" = 1 ]; then
    SCRATCH_DRY=1
    say "+ mktemp -d $ORIG_TMP/${SCRATCH_PREFIX}XXXXXX   (the scratch place; its env.sh holds every MAKIBA_* variable, XDG_* and TMPDIR, moved into it)"
    return 0
  fi
  RUN=$(mktemp -d "$ORIG_TMP/${SCRATCH_PREFIX}XXXXXX")
  : > "$RUN/.created-by-self-hosting-runbook"
  local port session=makiba-rehearsal-$$
  port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
  mkdir -p "$RUN/tmp"
  # Every variable a place is read from is set here, so nothing of the person's own places is reachable.
  # GH_CONFIG_DIR stays where it is: the GitHub CLI keeps its stored login under XDG_CONFIG_HOME otherwise.
  GH_CONFIG_DIR=${GH_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/gh}
  cat > "$RUN/env.sh" <<EOF
export RUN='$RUN' MK='$MK' GH_CONFIG_DIR='$GH_CONFIG_DIR'
export XDG_CONFIG_HOME='$RUN/config' XDG_STATE_HOME='$RUN/xdg-state' TMPDIR='$RUN/tmp'
export MAKIBA_STATE_DIR='$RUN/state' MAKIBA_PERSON_KEY='$RUN/person/person.key' MAKIBA_GATE_KEY='$RUN/person/gate.key'
export MAKIBA_WORKERS_DIR='$RUN/workers' MAKIBA_DOCS_DIR='$RUN/docs' MAKIBA_REPOS_DIR='$RUN/repos'
export MAKIBA_SHARE_DIR='$RUN/share' MAKIBA_GOALS_DIR='$RUN/config/makiba/goals'
export MAKIBA_DOOR='$RUN/door.sock' MAKIBA_PORT='$port' MAKIBA_BASE_SESSION='$session'
EOF
  say "scratch place: $RUN (env: $RUN/env.sh)"
  SCRATCH_RUN=$RUN
  # shellcheck source=/dev/null
  . "$RUN/env.sh"
  unset MAKIBA_BASE MAKIBA_LAUNCH_AGENT MAKIBA_FAKE_AGENT
}

# scratch_holders: the pids of processes (other than this one) that carry this scratch place's base session
# (HC_SESSION=<session>, as the core and its terminals do) or its folder in their command line. Read only: it
# looks at /proc of this user and touches no session. One pid per line; nothing printed when none is left.
scratch_holders() {
  python3 -I -c 'import os, sys
session, run = sys.argv[1].encode(), sys.argv[2].encode()
me = os.getpid()
for d in os.listdir("/proc"):
    if not d.isdigit() or int(d) == me:
        continue
    try:
        env = open("/proc/%s/environ" % d, "rb").read().split(b"\0")
        cmd = open("/proc/%s/cmdline" % d, "rb").read()
    except OSError:
        continue
    if b"HC_SESSION=" + session in env or (run and run in cmd):
        print(d)' "${MAKIBA_BASE_SESSION:-}" "${RUN:-}"
}

# stop_org: ends the organization of this shell's environment (makiba down). 0: it is stopped, or never ran
# here (no state.db), or makiba down failed but the place is SHOWN to hold nothing (see below); 1: makibad is
# still alive, or down failed and that cannot be shown (its terminals or base may then remain, and the place is
# needed to retry). makiba down exits non-zero when there is no daemon to talk to, so its exit code alone does
# not say the place is in use: a failed down is ignored only when all of these hold: status says makibad is not
# running, the pid it recorded is gone, and no process carries the place's base session or folder.
stop_org() {
  local pid st rc=0
  "$MK" down --secretary --stop-base >/dev/null 2>&1 || rc=$?
  if [ ! -f "$(db_path)" ]; then return 0; fi
  st=$("$MK" status 2>/dev/null || true)
  pid=$(printf '%s\n' "$st" | sed -n 's/^makibad: pid \([0-9][0-9]*\)$/\1/p')
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    sleep 3
    if kill -0 "$pid" 2>/dev/null; then return 1; fi
  fi
  if [ "$rc" -eq 0 ]; then return 0; fi
  # down failed: removable only on evidence (the pid, if any, is gone: checked above)
  printf '%s\n' "$st" | grep -q '^makibad is not running' || return 1
  [ -z "$(scratch_holders)" ] || return 1
  say "  makiba down exited $rc, but makibad is shown not to run here (status, its pid gone, no process holds the base session or the folder): the place is released"
  return 0
}

# Stops only the organization whose environment this shell holds (the scratch one), then keeps the evidence.
scratch_down() {
  say "== stopping the scratch organization"
  if ! stop_org; then check scratch-shutdown FAIL "makibad of the scratch place still runs after makiba down; the place is kept so you can retry: . $SCRATCH_RUN/env.sh; $MK down --secretary --stop-base"; fi
  if [ -f "$(db_path)" ]; then
    sq "SELECT id, occurred_at, kind, actor FROM events ORDER BY id" > "${RESULT%.txt}-events.txt" 2>/dev/null || true
    sq "SELECT id, sender, recipient, kind, substr(body, 1, 300) FROM messages ORDER BY id" > "${RESULT%.txt}-messages.txt" 2>/dev/null || true
    say "  events and messages kept next to this file (-events.txt, -messages.txt)"
  fi
}

remove_scratch() { # remove_scratch <dir>: only a folder this script made, by name and marker
  local d=$1
  case "$d" in "$ORIG_TMP/$SCRATCH_PREFIX"??????) ;; *) say "  not a scratch place of this script: $d"; return 0 ;; esac
  [ -f "$d/.created-by-self-hosting-runbook" ] || { say "  no marker in $d: left alone"; return 0; }
  rm -rf "$d"
  say "  removed $d"
}

stage_rehearse() {
  say "== rehearsal in a scratch place (no real place is written; the GitHub repository is only read)"
  say "Left behind on purpose by any run: trust entries for the scratch path in ~/.claude.json and a transcript folder under ~/.claude/projects/ (runbook section 1)."
  preflight scratch
  scratch_env
  if [ "$DRY" = 1 ]; then
    plan "NOTE: the paths printed below are the real defaults; a real rehearsal points every one of them into the scratch place"
    plan "run everything below with that environment, then: $MK down --secretary --stop-base, keep events and messages, remove the scratch folder (kept if a check failed)"
  fi
  stage_register 1
  stage_triage 1
}

# ---- first-day (Issue 51): the four checks before makiba's first range, in a scratch place ----

FD_SUM=()    # one line per check, printed last
FD_NEXT=()   # what to do next, one per FAIL
FD_GAPS=()   # what no script shows (named steps) or what stays unproven
FD_READER=""     # the reader the newest diff review of this stage used
FD_VERDICT=""    # its verdict
FD_DONE=0        # 1 once every check was made: a stage that stopped earlier is never READY
# The words a refused or unreachable host leaves in cargo's, git's or curl's own text.
NET_ERR='resolve host|resolve hostname|[Ff]ailed to connect|onnection refused|CONNECT|[Pp]roxy|tunnel|spurious network error|failed to download|unable to update registry'

# fd <name> <PASS|FAIL|INFO> <detail> [<what to do next, for a FAIL>]
fd() {
  check "$1" "$2" "$3"
  local one=${3%%$'\n'*}
  FD_SUM+=("$1: $2  ${one:0:220}")
  if [ "$2" = FAIL ]; then FD_NEXT+=("$1: ${4:-read the lines above}"); fi
  return 0
}

# The last lines of the results: one line per check, what to do for each FAIL, one verdict to relay as it is.
fd_summary() { # fd_summary <exit status of the stage>
  if [ "$STAGE" != first-day ] || [ "$DRY" = 1 ] || [ "${#FD_SUM[@]}" -eq 0 ]; then return 0; fi
  local l v
  say "== summary (first-day, commit $COMMIT)"
  for l in "${FD_SUM[@]}"; do say "SUMMARY $l"; done
  for l in "${FD_NEXT[@]+"${FD_NEXT[@]}"}"; do say "NEXT $l"; done
  for l in "${FD_GAPS[@]+"${FD_GAPS[@]}"}"; do say "NOT SHOWN $l"; done
  if [ "${1:-0}" -ne 0 ] && [ "$FD_DONE" != 1 ]; then v="NOT READY: the run stopped before every check was made (exit $1; the lines above say why); fix that and run the line again"
  elif [ "$FD_DONE" != 1 ]; then v="NOT READY: the run ended before every check was made; run the line again"
  elif [ "$FAILS" -gt 0 ]; then v="NOT READY: $FAILS check(s) failed; the NEXT lines say what to do, then run the line again"
  elif [ "${#FD_GAPS[@]}" -gt 0 ]; then v="READY on what a script can show; left for the person: the NOT SHOWN lines"
  else v="READY: every check passed"; fi
  say "VERDICT first-day: $v"
}

# The real organization's makibad, read only: its pid is in state.db (makiba status reads it without makibad).
fd_real_binary() {
  say "== check 1: the binary"
  say "+ $MK status        (read only: which makibad the real organization runs; nothing is changed)"
  if [ "$DRY" = 1 ]; then
    plan "PASS: this checkout built as $COMMIT; INFO: the real makibad's pid, the program it runs (/proc/<pid>/exe) and when it started"
    plan "print the two lines that restart the real organization on this build: $MK down, then $MK up"
    return 0
  fi
  if [ -n "$MK_GIVEN" ]; then fd binary INFO "$MK was named with MK= and is used as it is; this checkout is $COMMIT"
  else fd binary PASS "built $COMMIT: $MK ($(stat -c %y "$MK" | cut -c1-19))"; fi
  local pid exe when
  pid=$("$MK" status 2>/dev/null | sed -n 's/^makibad: pid \([0-9][0-9]*\)$/\1/p' || true)
  exe=""; if [ -n "$pid" ]; then exe=$(readlink "/proc/$pid/exe" 2>/dev/null || true); fi
  if [ -n "$exe" ]; then
    when=$(ps -o lstart= -p "$pid" 2>/dev/null || true)
    fd real-binary INFO "the real makibad (pid $pid) runs $exe, started $when; a binary carries no commit (its version is 0.0.0), so what it was built from cannot be read; this checkout is $COMMIT, committed $(git -C "$REPO" log -1 --format=%cI)"
  else
    fd real-binary INFO "no real makibad could be read (makiba status shows no pid on the default places): nothing to compare, the real organization is not running on this build"
  fi
  say "  to restart the real organization on this build, type these two lines in a login shell, one after the other (the second brings back the runs the first suspended):"
  say "    $MK down"
  say "    $MK up"
}

# fd_brief <task> <full|doc>: the brief of one tiny code task. ASCII only.
fd_brief() {
  local t=$1 mode=$2
  printf 'This is a check that a code worker of makiba works end to end. Keep it tiny. Work only inside your run repository (your current folder). Never push.\n'
  if [ "$mode" = full ]; then
    printf '1. Run: CARGO_HOME="$PWD/.cargo-home" cargo fetch 2>&1 | tail -n 12   (a CARGO_HOME of its own, so that cargo has no cache and must download from the network hosts; put it before every cargo command, each Bash call starts fresh).\n'
    printf '2. Run: ls "$PWD/.cargo-home/registry/cache"/*/ | wc -l   (this number is DOWNLOADED).\n'
    printf '3. Run: CARGO_HOME="$PWD/.cargo-home" cargo build 2>&1 | tail -n 15\n'
  fi
  printf 'Then append the single line "First-day check: a makiba code worker made this line." to README.md, and create tests/first_day_check.rs holding exactly one test: #[test] fn first_day_check() { assert_eq!(2 + 2, 4); } (write it over several lines).\n'
  if [ "$mode" = full ]; then
    printf 'Run: CARGO_HOME="$PWD/.cargo-home" cargo test --test first_day_check 2>&1 | tail -n 15\n'
  fi
  printf 'Then: git add README.md tests/first_day_check.rs (only these two; never .cargo-home or target) and git commit -m "docs: first-day check".\n'
  printf 'Then call the tool review exactly once, with the single argument kind set to "diff" ({"kind":"diff"}) and nothing else: no task, no allowed, no reason (it is your own task, the door knows it, and an extra argument is refused). Wait for its answer (it can take up to 20 minutes). Do not call it again.\n'
  if [ "$mode" = full ]; then
    printf 'Last, call the tool report with state "done", task "%s" and one sentence that starts with: BUILD=<ok|failed> DOWNLOADED=<the number> TEST=<ok|failed>, then what the review answered in a few words.\n' "$t"
    printf 'If cargo fails with a network, host, connection or proxy error, do not retry: call the tool report with state "blocked", task "%s" and the first 8 lines of that error exactly as printed. The same if report done is refused.\n' "$t"
  else
    printf 'Last, call the tool report with state "done", task "%s" and one sentence that says what the review answered. Run no cargo command. If report done is refused, call report with state "blocked" and the refusal text.\n' "$t"
  fi
}

# fd_dispatch <task> <brief file>: dispatches the task and waits for its done or blocked report, or a failed launch.
# FD_OUTCOME: done, blocked, failed (the launch) or timeout; FD_BODY: the report's text or the launch's reason.
fd_dispatch() {
  local task=$1 f=$2 limit=${WAIT_CODE:-3000}
  FD_OUTCOME=timeout; FD_BODY=""
  say "+ $MK dispatch --kind code --project $SLUG --task-id $task --title 'First-day check' --brief-file $f"
  "$MK" dispatch --kind code --project "$SLUG" --task-id "$task" --title "First-day check" --brief-file "$f" || { FD_OUTCOME=failed; FD_BODY="makiba dispatch exited non-zero (its text is above)"; return 0; }
  settled() { [ -n "$(sq "SELECT 1 FROM messages WHERE kind IN ('report_done','report_blocked') AND run_id = (SELECT id FROM runs WHERE task_id = '$task') UNION ALL SELECT 1 FROM run_launch l JOIN runs r ON r.id = l.run_id WHERE r.task_id = '$task' AND l.stage = 'failed'")" ]; }
  say "  this wait may take up to $((limit / 60)) minutes (a cold cargo fetch and build, the worker's turns and its diff review)"
  wait_for "$limit" "the worker's done or blocked report for $task" settled || return 0
  FD_BODY=$(sq "SELECT kind, body FROM messages WHERE kind IN ('report_done','report_blocked') AND run_id = (SELECT id FROM runs WHERE task_id = '$task') ORDER BY id DESC LIMIT 1")
  case "$FD_BODY" in
    report_done\|*) FD_OUTCOME=done ;;
    report_blocked\|*) FD_OUTCOME=blocked ;;
    *) FD_OUTCOME=failed; FD_BODY=$(sq "SELECT coalesce(l.failed_reason, '') FROM run_launch l JOIN runs r ON r.id = l.run_id WHERE r.task_id = '$task' AND l.stage = 'failed' ORDER BY l.id DESC LIMIT 1") ;;
  esac
  FD_BODY=${FD_BODY#*|}
  return 0
}

# Host names a failure text names (after "host" or "://"), not the ones the project already lists.
fd_hosts_in() {
  printf '%s\n' "$1" | grep -E "$NET_ERR" | grep -oE '(://|[Hh]ost:? ?|connect to )[A-Za-z0-9.-]+\.[A-Za-z]{2,}' | sed -E 's#^(://|[Hh]ost:? ?|connect to )##' | sort -u | grep -vxF -f <(printf '%s\n' "${HOSTS//,/$'\n'}") || true
}

# Check 2, read from the worker's own messages and the run's events (no event names a refused host:
# the refusal is Claude Code's, so it shows in the worker's words, which the brief asks it to quote).
fd_check_build() {
  local task=$1 hosts texts n
  texts=$(sq "SELECT body FROM messages WHERE run_id = (SELECT id FROM runs WHERE task_id = '$task') UNION ALL SELECT payload_json FROM events WHERE run_id = (SELECT id FROM runs WHERE task_id = '$task')")
  hosts=$(fd_hosts_in "$FD_BODY
$texts" | tr '\n' ' ')
  hosts=${hosts% }
  if [ -n "$hosts" ]; then
    fd code-worker-build FAIL "a worker's network host was refused: $hosts (not in the project's list: $HOSTS)" "add it, then run the line again: MAKIBA_RUNBOOK_HOSTS=$HOSTS,${hosts// /,} bash scripts/runbooks/self-hosting.sh first-day ; and for the real organization: $MK config set network_hosts --project $SLUG $HOSTS,${hosts// /,}"
    return 0
  fi
  case "$FD_OUTCOME" in
    timeout) fd code-worker-build FAIL "no done report in ${WAIT_CODE:-3000} s" "read its screen with $MK peek $task, then end it: $MK close $task --abandon" ;;
    failed) fd code-worker-build FAIL "the launch failed: ${FD_BODY:-no reason recorded}" "docs/runbooks/self-hosting.md section 7 and the screen in worker_spawn_failed ($MK events); then run the line again" ;;
    done | blocked)
      # The build is judged alone: a worker that built and tested and then reported blocked for another reason
      # (its review) still passes here; review-diff carries what it said.
      n=$(printf '%s' "$FD_BODY" | grep -oE 'DOWNLOADED=[0-9]+' | head -n 1 | cut -d= -f2 || true)
      if ! printf '%s' "$FD_BODY" | grep -q 'BUILD=ok' || ! printf '%s' "$FD_BODY" | grep -q 'TEST=ok'; then
        fd code-worker-build FAIL "the $FD_OUTCOME report does not say BUILD=ok and TEST=ok: ${FD_BODY%%$'\n'*}" "read its whole report ($MK show <message id>; the results file keeps it) and the build output it quotes; run the line again"
      elif [ "${n:-0}" -gt 0 ]; then
        fd code-worker-build PASS "$FD_OUTCOME report with BUILD=ok TEST=ok after a cold download of $n crates (a CARGO_HOME of its own); no refused host in its messages or events"
      else
        fd code-worker-build INFO "built from a warm cache: the hosts are not proven (the report says DOWNLOADED=${n:-none}; ${FD_BODY%%$'\n'*})"
        FD_GAPS+=("code-worker-build: the network hosts were not exercised (no crate was downloaded); run the line again, or watch the first real code worker's first cargo build")
      fi ;;
  esac
}

# Check 3, read from the answer the door stored (commands) and the round's event; the reason whole.
fd_check_review() { # fd_check_review <task> <check name> <expected reader: any|fresh>
  local task=$1 name=$2 want=$3 row verdict reader round reason fb st
  row=$(sq "SELECT coalesce(json_extract(result_json, '\$.result.verdict'), ''), coalesce(json_extract(result_json, '\$.result.reader'), ''), coalesce(json_extract(result_json, '\$.result.round'), '') FROM commands WHERE verb = 'review' AND actor = 'w-$task' AND status = 'done' AND json_extract(result_json, '\$.result.kind') = 'diff' ORDER BY id DESC LIMIT 1")
  FD_READER=""; FD_VERDICT=""
  say "  codex_round events of $task:"
  sq "SELECT id, kind, payload_json FROM events WHERE kind IN ('codex_round_started', 'codex_round_denied') AND json_extract(payload_json, '\$.task') = '$task' ORDER BY id" | sed 's/^/    | /' | tee -a "${RESULT:-/dev/null}" || true
  if [ -z "$row" ]; then
    st=$(sq "SELECT status || ' ' || coalesce(error_kind, '') FROM commands WHERE verb = 'review' AND actor = 'w-$task' ORDER BY id DESC LIMIT 1")
    fd "$name" FAIL "no answered diff review of $task (the worker's review command: ${st:-none was made})" "read the worker's report ($MK show) and its screen ($MK peek $task); the door's rows: the commands table, verb review"
    return 0
  fi
  IFS='|' read -r verdict reader round <<EOF
$row
EOF
  FD_READER=$reader; FD_VERDICT=$verdict
  reason=$(sq "SELECT coalesce(json_extract(result_json, '\$.result.reason'), '') FROM commands WHERE verb = 'review' AND actor = 'w-$task' AND status = 'done' AND json_extract(result_json, '\$.result.kind') = 'diff' ORDER BY id DESC LIMIT 1")
  fb=$(sq "SELECT coalesce(json_extract(result_json, '\$.result.fallback_reason'), '') FROM commands WHERE verb = 'review' AND actor = 'w-$task' AND status = 'done' AND json_extract(result_json, '\$.result.kind') = 'diff' ORDER BY id DESC LIMIT 1")
  if [ "$verdict" = passed ]; then
    if [ "$want" = fresh ] && [ "$reader" != fresh-session ]; then
      fd "$name" FAIL "passed by $reader, not by the fresh session: hiding the other model's program did not take effect" "check MAKIBA_REVIEW_OTHER in makibad's environment (src/daemon/review.rs, Programs::from_env) and run the line again"
    else
      fd "$name" PASS "verdict passed, reader $reader, round $round${fb:+ (fallback_reason: $fb)}"
    fi
  else
    say "  the review's reason, whole (Issue 280: it lists the commands the reader ran; this list tunes the recognizer):"
    printf '%s\n' "$reason" | sed 's/^/    | /' | tee -a "${RESULT:-/dev/null}"
    fd "$name" FAIL "verdict $verdict, reader $reader, round $round; the whole reason is in the results file above" "read the reason above: a reader that opened none of the change's files names the commands it ran there; fix the recognizer (docs/design/lines.md L-K10, src/daemon/review.rs) or the reader's setup, then run the line again"
  fi
}

# Check 4: the hook's answer, from a stand-in door that answers as the gate does (a head with no passed review, mode human).
fd_check_gate_ask() {
  say "== check 4: the landing gate's question (the hook's answer; nothing is landed, no pull request exists)"
  local sha=0123456789abcdef0123456789abcdef01234567 cmd payload out rc
  cmd="gh pr merge 1 --repo $GH_REPO --squash --match-head-commit $sha"
  payload=$(python3 -I -c 'import json, sys; print(json.dumps({"tool_name": "Bash", "tool_input": {"command": sys.argv[1]}}))' "$cmd")
  say "+ the payload of the secretary's allowed landing command: $cmd"
  if [ "$DRY" = 1 ]; then
    plan "start a stand-in door in the scratch place that answers the gate's request with allow, warn and a reason with WARNING (a head with no diff review, mode human); run: $MK hook merge-gate with the payload and that door; read permissionDecision and WARNING"
    plan "ask the scratch organization's real door once with the same command for a pull request that does not exist (expects a refusal that names the acceptance gate: shows the gate key and the door answer)"
    plan "print the named step for the person"
    return 0
  fi
  cat > "$RUN/standin-door.py" <<'EOF'
import json, os, socket, sys
path, key = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
s.bind(path)
s.listen(1)
c, _ = s.accept()
req = json.loads(c.makefile().readline())
ok = req.get("key") == key and req.get("verb") == "gate"
head = req.get("args", {}).get("head", "")[:12]
reason = "no declared acceptance test; WARNING review of %s: none (no diff review was run): the person decides this landing" % head
res = {"verdict": "no_manifest", "allow": True, "warn": True, "reason": reason, "mode": "human"}
out = {"ok": True, "result": res} if ok else {"ok": False, "error": {"message": "stand-in: wrong key or verb"}}
c.sendall((json.dumps(out) + "\n").encode())
EOF
  printf 'stand-in-gate-key\n' > "$RUN/standin-gate.key"
  python3 -I "$RUN/standin-door.py" "$RUN/standin-door.sock" stand-in-gate-key &
  local sp=$! i=0
  while [ ! -S "$RUN/standin-door.sock" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
  rc=0
  out=$(printf '%s' "$payload" | MAKIBA_DOOR="$RUN/standin-door.sock" MAKIBA_GATE_KEY="$RUN/standin-gate.key" "$MK" hook merge-gate 2>&1) || rc=$?
  kill "$sp" 2>/dev/null || true
  wait "$sp" 2>/dev/null || true
  say "  the hook (stand-in door): exit $rc, output: $out"
  if [ "$rc" -eq 0 ] && printf '%s' "$out" | python3 -I -c 'import json, sys; o = json.load(sys.stdin)["hookSpecificOutput"]; assert o["permissionDecision"] == "ask" and "WARNING" in o["permissionDecisionReason"]'; then
    fd gate-ask PASS "the hook answered permissionDecision ask with a reason holding WARNING, for the allowed landing command under a stand-in door that answers as the gate does for a head with no review. This proves the hook's answer only: not the gate's decision on a real head, and not that Claude Code shows the question"
  else
    fd gate-ask FAIL "the hook did not answer ask with WARNING (exit $rc): $out" "src/hooks/merge_gate.rs ask_gate; run the line again and read the output above"
  fi
  rc=0
  out=$(printf '%s' "$payload" | "$MK" hook merge-gate 2>&1) || rc=$?
  say "  the hook (the scratch organization's real door, a pull request that does not exist): exit $rc, output: $out"
  case "$out" in
    *ACCEPTANCE_GATE*) fd gate-door INFO "the scratch door answered the hook with the gate key: ${out%%$'\n'*}" ;;
    *) fd gate-door FAIL "the scratch door did not answer the gate request as a refusal of the gate (exit $rc): $out" "is makibad running, is MAKIBA_GATE_KEY read (spec section 3)? run the line again" ;;
  esac
  FD_GAPS+=("gate-ask: that Claude Code shows the question to the person is not shown by this script. Named step for the person: in the real secretary's session, when the first pull request of a code task is to be landed with a head that has no passed diff review, the secretary runs: gh pr merge <n> --repo $GH_REPO --squash --match-head-commit <full head sha> ; look for a question carrying WARNING review of <head>: none. If the command runs with no question, that is a failure of the hook's wiring: write it in the results")
}

stage_first_day() {
  say "== first-day checks: what must hold on the real host before makiba's first range (Issue 51), in a scratch place"
  say "The real organization is only read, once (makiba status). Nothing is pushed, landed or opened as a pull request; GitHub is read. Left behind on purpose, as by the rehearsal: trust entries in ~/.claude.json and transcript folders (runbook section 1)."
  say "Waits (printed again when each starts): the clone of makiba up to $((${WAIT_CLONE:-1800} / 60)) min; each code worker's report up to $((${WAIT_CODE:-3000} / 60)) min (a cold cargo fetch and build is most of it; the diff review inside is bounded at 20 min). Expected total 25 to 60 minutes."
  say "Cost: one real code worker and one diff review; a second worker and review only when the first review was read by the other model's program."
  preflight scratch
  fd_real_binary
  scratch_env
  if [ "$DRY" = 1 ]; then
    plan "NOTE: the paths printed below are the real defaults; a real run points every one of them into the scratch place, with MAKIBA_MERGE_APPROVAL_MODE=human (the scratch organization only)"
  else
    export MAKIBA_MERGE_APPROVAL_MODE=human
  fi
  stage_register 1
  say "== check 2 and 3: one real code worker, in the scratch organization"
  local stamp A B f
  stamp=$(date -u +%y%m%d%H%M); A=fda-$stamp; B=fdb-$stamp
  if [ "$DRY" = 1 ]; then
    say "+ $MK dispatch --kind code --project $SLUG --task-id $A --title 'First-day check' --brief-file <brief: cargo fetch with a CARGO_HOME of its own, cargo build, one README line, one trivial test, cargo test, commit, review kind diff, report>"
    plan "wait for the worker's done or blocked report (up to $((${WAIT_CODE:-3000} / 60)) minutes); PASS only on BUILD=ok TEST=ok with DOWNLOADED above 0 and no refused host in its messages and events; a refused host is a FAIL that names it and the line that adds it"
    plan "read the stored answer of the worker's review and the codex_round events: PASS when passed (with the reader), FAIL when not-established with the whole reason copied"
    plan "when the reader was not the fresh session: $MK close $A --abandon; $MK down; MAKIBA_REVIEW_OTHER=<a path that does not exist> $MK up --no-secretary (this scratch organization only); dispatch $B (a README line and the trivial test, no cargo); expect passed by fresh-session"
    fd_check_gate_ask
    plan "print one line per check, a NEXT line for each FAIL, the NOT SHOWN lines and one VERDICT line"
    return 0
  fi
  [ -n "$(sq "SELECT 1 FROM projects WHERE slug = '$SLUG'")" ] || refuse "project $SLUG is not registered in the scratch organization" "read the check lines above and makibad.log of the scratch state folder, then run the line again"
  f=$RUN/brief-$A.md; fd_brief "$A" full > "$f"
  fd_dispatch "$A" "$f"
  say "  the worker's report: ${FD_OUTCOME}: $FD_BODY"
  fd_check_build "$A"
  if [ "$FD_OUTCOME" = done ]; then
    fd_check_review "$A" review-diff any
  else
    if [ "$FD_OUTCOME" = blocked ] && printf '%s' "$FD_BODY" | grep -qi 'classifier'; then
      fd review-diff FAIL "a door tool call of the worker (review) was denied by the Claude Code auto mode classifier before it reached makibad; its words: ${FD_BODY%%$'\n'*}" "Issue 292 (a role's settings allow its own door tools); until it lands the review cannot run here: run the line again after it, or read $MK show <message id>"
    elif [ "$FD_OUTCOME" = blocked ]; then
      fd review-diff FAIL "the worker reported blocked and made no review; its words: ${FD_BODY%%$'\n'*}" "read its whole report ($MK show <message id>; the results file keeps it), then run the line again"
    else
      fd review-diff FAIL "no review to read: the worker did not get to its done report ($FD_OUTCOME)" "see code-worker-build above"
    fi
  fi
  case "$FD_READER" in
    "" | none) fd review-fresh-session INFO "not run: the first review gave no reader to compare with" ;;
    fresh-session) fd review-fresh-session INFO "not run: the first review was already read by the fresh session" ;;
    *)
      say "== check 3, second reading: the other model's program ($FD_READER) hidden from the scratch makibad's lookup, a second worker"
      say "+ $MK close $A --abandon   (the scratch run repository is never pushed)"
      "$MK" close "$A" --abandon || true
      say "+ $MK down"
      "$MK" down
      export MAKIBA_REVIEW_OTHER="$RUN/reader-hidden-for-this-check"   # a path that is not there: src/daemon/review.rs resolves a path as it is
      say "+ MAKIBA_REVIEW_OTHER=$MAKIBA_REVIEW_OTHER $MK up --no-secretary   (the scratch organization only)"
      "$MK" up --no-secretary
      f=$RUN/brief-$B.md; fd_brief "$B" doc > "$f"
      fd_dispatch "$B" "$f"
      say "  the worker's report: ${FD_OUTCOME}: $FD_BODY"
      if [ "$FD_OUTCOME" = done ]; then fd_check_review "$B" review-fresh-session fresh
      else fd review-fresh-session FAIL "no review to read: the second worker did not get to its done report ($FD_OUTCOME): ${FD_BODY%%$'\n'*}" "$MK peek $B, then $MK close $B --abandon; run the line again" ; fi
      ;;
  esac
  fd_check_gate_ask
  FD_DONE=1
}

# ---- switch (Issue 51): the real organization restarted on origin/main, the count begun ----

# The pieces of `status --json` the switch reads: one word, then one line per run in work, per run
# that only waits (suspended or failed) and per pending entry, then the secretary's terminal state.
sw_status() {
  "$MK" status --json 2>/dev/null | python3 -I -c 'import json, sys
v = json.load(sys.stdin)
print("running" if v["makibad"]["running"] else "stopped")
for r in v["runs"]:
    kind = "wait" if r["status"] in ("suspended", "failed") else "work"
    print("%s run %s (%s, %s)" % (kind, r["task_id"], r["kind"], r["status"]))
for p in v["pending"]:
    q = (p["question"] or "").splitlines()
    print("work pending entry %s of %s: %s" % (p["id"], p["task_id"] or "-", q[0] if q else "-"))
for t in v["terminals"]:
    if t["role"] == "secretary":
        print("secretary %s" % (t["state"] or "no state yet"))'
}

# sw_fail <what failed> <the one line that recovers>: the organization may be down; the next line is named.
sw_fail() {
  check switch FAIL "$1"
  say "RECOVER: $2"
  say "RESULT self-hosting switch: failed (the RECOVER line above is the next line to type)"
  exit 1
}

# The merge approval mode (spec section 2): only the environment variable MAKIBA_MERGE_APPROVAL_MODE sets it
# (`config set merge_approval_mode` is "not built yet" and `config get` always answers null), read by makibad
# from its own environment, which makiba up passes on from the shell that runs it. Trimmed, any case; anything
# that is not "acceptance" is read as human. The first range runs under human (the owner's decision of 2026-11-22),
# so the switch accepts only unset or human here, and refuses everything else before anything is stopped.
sw_mode_ok() {
  local m; m=$(printf '%s' "${MAKIBA_MERGE_APPROVAL_MODE-human}" | tr 'A-Z' 'a-z' | tr -d '[:space:]')
  [ "$m" = human ]
}
# The mode of a running makibad, from its own environment (/proc/<pid>/environ of the pid status shows): human,
# acceptance, the word "unset", or "unreadable".
sw_mode_of_makibad() {
  local pid
  pid=$("$MK" status 2>/dev/null | sed -n 's/^makibad: pid \([0-9][0-9]*\)$/\1/p')
  [ -n "$pid" ] && [ -r "/proc/$pid/environ" ] || { printf unreadable; return 0; }
  tr '\0' '\n' < "/proc/$pid/environ" | sed -n 's/^MAKIBA_MERGE_APPROVAL_MODE=//p' | head -n 1 | tr 'A-Z' 'a-z' | tr -d '[:space:]' | grep . || printf unset
}

sw_preflight() {
  say "== preconditions (switch)"
  # No build here: the build of the new revision waits until makibad is stopped (a binary replaced under a running makibad leaves its hook and channel paths stale).
  must "not inside an agent" "a CLAUDE* or HC_SESSION/HC_SOCKET/HC_SCOPE/HC_TERMINAL_* variable is set: this shell is inside an agent" "open a plain login shell on the host and run the line again" not_in_agent
  for c in git cargo gh "${HC_BIN:-hc}" claude python3; do
    must "$c on PATH" "$c is not on PATH" "install it, or put it on PATH" have "$c"
  done
  must "git 2.38 or newer" "git is older than 2.38 (git merge-tree --write-tree)" "install git 2.38 or newer" git_new_enough
  must "stored login for github.com" "the GitHub CLI has no stored login for github.com" "run: gh auth login -h github.com" gh_login
  if [ "$DRY" != 1 ]; then
    for v in MAKIBA_BASE MAKIBA_LAUNCH_AGENT MAKIBA_FAKE_AGENT; do
      if [ -n "${!v:-}" ]; then refuse "$v is set (a test seam)" "unset $v and run the line again"; fi
    done
    [ "${MAKIBA_DOOR:-}" != none ] || refuse "MAKIBA_DOOR=none is set (a test seam)" "unset MAKIBA_DOOR and run the line again"
    [ -x "$MK" ] || refuse "$MK is not an executable" "name the installed binary with MK=<path>, or build once: cargo build"
  fi
}

stage_switch() {
  say "== switch: the real organization restarted on origin/main, with the secretary; the count of Issue 51 begins"
  local state sec_dir hand start mode old new
  state=$(dirname "$(db_path)"); sec_dir=$state/secretary; hand=$sec_dir/handover-owner.md; start=$state/self-hosting-start.txt
  mode=human
  sw_preflight
  say "+ the merge approval mode of this shell: MAKIBA_MERGE_APPROVAL_MODE=${MAKIBA_MERGE_APPROVAL_MODE-<unset>} (makibad reads it from the shell that runs makiba up; the first range runs under human)"
  if [ "$DRY" != 1 ] && ! sw_mode_ok; then
    refuse "MAKIBA_MERGE_APPROVAL_MODE is '$MAKIBA_MERGE_APPROVAL_MODE' in this shell; the first range starts under the human mode (owner's decision of 2026-11-22), and makibad would read this value from the shell that runs makiba up" "unset MAKIBA_MERGE_APPROVAL_MODE   (then run this line again; only this variable sets the mode: config set merge_approval_mode is not built)"
  fi

  say "== what is read first (nothing is changed before you confirm)"
  say "+ $MK status --json     (is makibad running; a run in work or a pending entry unanswered refuses)"
  say "+ git -C $REPO fetch origin; git -C $REPO merge-base --is-ancestor HEAD origin/main   (the checkout must be on main, clean, and able to fast-forward)"
  if [ -n "$SWITCH_HANDOVER" ]; then say "+ the handover $SWITCH_HANDOVER must exist, be non-empty and lie outside $REPO"; fi
  if [ "$DRY" != 1 ]; then
    local st work l; st=$(sw_status) || true
    [ -n "$st" ] || refuse "makiba status --json gave no readable answer" "run: $MK status ; there is no organization state to read on this place"
    work=$(printf '%s\n' "$st" | sed -n 's/^work //p')
    if [ -n "$work" ]; then
      say "in work in the real organization:"
      printf '%s\n' "$work" | while IFS= read -r l; do say "  - $l"; done
      refuse "the organization has work in it (a run in use or a pending entry unanswered); a restart would suspend or strand it" "let the secretary finish or close them (and answer the pending entries), then run this line again"
    fi
    printf '%s\n' "$st" | sed -n 's/^wait //p' | while IFS= read -r l; do say "  note: $l (not in use; makiba up brings a suspended run back on the new build)"; done
    check organization INFO "makibad $(printf '%s\n' "$st" | head -n 1); no run in use, no pending entry unanswered"
    [ "$(git -C "$REPO" rev-parse --abbrev-ref HEAD)" = main ] || refuse "$REPO is not on main" "git -C $REPO switch main"
    [ -z "$(git -C "$REPO" status --porcelain --untracked-files=no)" ] || refuse "$REPO has uncommitted changes to tracked files" "save or discard them in that checkout, then run this line again"
    git -C "$REPO" fetch origin >/dev/null 2>&1 || refuse "git fetch origin failed in $REPO" "check the network and the stored login (gh auth status), then run this line again"
    git -C "$REPO" merge-base --is-ancestor HEAD origin/main || refuse "$REPO holds changes origin/main lacks: it cannot fast-forward" "move them to a branch yourself (git -C $REPO branch <name>), put main on origin/main, then run this line again; this script never resets or merges"
    old=$(git -C "$REPO" rev-parse --short HEAD); new=$(git -C "$REPO" rev-parse --short origin/main)
    check checkout INFO "$REPO: main $old, origin/main $new; a fast-forward is possible"
    case "$(realpath -m "$state")/" in "$(realpath "$REPO")"/*) refuse "the state folder $state lies inside this repository ($REPO), which is public: the handover and the start file would be placed in it" "unset MAKIBA_STATE_DIR or point it outside the checkout, then run this line again" ;; esac
    if [ -n "$SWITCH_HANDOVER" ]; then
      [ -s "$SWITCH_HANDOVER" ] || refuse "the handover $SWITCH_HANDOVER does not exist or is empty" "name the file the previous secretary wrote: --handover <file>"
      case "$(realpath "$SWITCH_HANDOVER")/" in "$(realpath "$REPO")"/*) refuse "the handover lies inside this repository ($REPO), which is public" "move it outside the checkout (for example your home folder) and name that path" ;; esac
    fi
  else
    plan "refuse and list the runs in use and the pending entries; refuse unless $REPO is on main, clean, and origin/main is a fast-forward of it"
  fi

  lines_changed "makibad and the worker terminals are stopped (makiba down --secretary: the secretary's terminal too, so the new one starts on the new role files; the base is NOT stopped)
the checkout $REPO moves to origin/main by a fast-forward and is built: the binary $MK is replaced
makiba up starts makibad and a new secretary; merge approval mode for makibad: human (this shell holds no other value)
$start is written once (the start of the count: time, revisions, the newest event id, the base version)
${SWITCH_HANDOVER:+the handover is copied to $hand (mode 0600)}
Kept as they are: the goal ledger already placed, the project registration, the clone, the keys, the base, every event and message."
  confirm

  # From here on every failure says the next line. The fast-forward below changes this script's own file, so
  # the case branch that calls this function ends in `exit`: the shell never reads the file again.
  say "== stop the organization"
  # makiba down waits until makibad has let go of its lock and ends the terminals itself (src/down.rs); its exit status
  # is the answer. (status shows "running" for 30 s after a stop, since it reads the last heartbeat: it is not used here.)
  run "$MK" down --secretary || sw_fail "makiba down failed (its text is above): makibad, the secretary's terminal or the base may not be stopped as asked" "$MK status ; if the organization is not as it was, $MK up brings it back; then run this stage again (the checkout and the binary were not touched)"
  if [ "$DRY" != 1 ]; then check down PASS "makiba down: makibad stopped, the secretary's terminal ended"; fi

  say "== bring the checkout to origin/main and build"
  run git -C "$REPO" merge --ff-only origin/main || sw_fail "the fast-forward failed" "$MK up   (the checkout and the old binary are as they were); read the error above, then run this stage again"
  if [ -z "$MK_GIVEN" ]; then
    run cargo build --manifest-path "$REPO/Cargo.toml" || sw_fail "cargo build failed at $(git -C "$REPO" rev-parse --short HEAD)" "$MK up   (the old binary is still in place and nothing was suspended; fix the build, then run this stage again)"
  else
    say "  $MK was named with MK=: it is used as it is, no build"
  fi
  if [ "$DRY" != 1 ]; then check build PASS "checkout $(git -C "$REPO" rev-parse --short HEAD), binary $MK ($(stat -c %y "$MK" | cut -c1-19))"; fi

  say "== start the organization with the secretary"
  local before=0
  if [ "$DRY" != 1 ]; then before=$(sq "SELECT coalesce(max(id), 0) FROM run_launch WHERE role = 'secretary'"); fi
  run "$MK" up || sw_fail "makiba up failed" "$MK up   (read its text above and makibad.log in $state; suspended runs stay suspended, nothing is lost)"
  sec_ready() { [ -n "$(sq "SELECT 1 FROM run_launch WHERE role = 'secretary' AND id > $before AND stage IN ('verified', 'failed')")" ]; }
  if [ "$DRY" = 1 ]; then
    plan "wait for a secretary launch newer than id $before to be 'verified' (up to ${WAIT_SECRETARY:-600} s), and for its terminal to be listed by status"
  else
    wait_for "${WAIT_SECRETARY:-600}" "the secretary's launch to be verified" sec_ready || sw_fail "the secretary was not ready in ${WAIT_SECRETARY:-600} s" "$MK up   (asks again; to read what happened: $MK events --after $before --json)"
    local stage reason
    stage=$(sq "SELECT stage FROM run_launch WHERE role = 'secretary' AND id > $before ORDER BY id DESC LIMIT 1")
    reason=$(sq "SELECT coalesce(failed_reason, '') FROM run_launch WHERE role = 'secretary' AND id > $before ORDER BY id DESC LIMIT 1")
    [ "$stage" = verified ] || sw_fail "the secretary's launch failed: $reason" "$MK up   (asks for the secretary again; its screen is in the worker_spawn_failed event: $MK events --after 0 --json)"
    sw_status | grep -q '^secretary ' || sw_fail "the launch is verified but status lists no secretary terminal" "$MK status ; then $MK up"
    check secretary PASS "the secretary's launch is verified; its terminal: $(sw_status | sed -n 's/^secretary //p')"
    mode=$(sw_mode_of_makibad)
    case "$mode" in
      human | unset) check merge-approval-mode PASS "makibad's own environment says: $mode (read as human)"; mode=human ;;
      unreadable) check merge-approval-mode INFO "makibad's environment cannot be read here; this shell held no value but human, which makiba up passed on; makiba range approve prints the mode it sees"; mode=human ;;
      *) sw_fail "makibad runs with MAKIBA_MERGE_APPROVAL_MODE=$mode, not human" "makiba down --secretary ; then unset MAKIBA_MERGE_APPROVAL_MODE ; then $MK up" ;;
    esac
  fi

  say "== the read surface and the verbs of the new binary"
  local v missing=""
  for v in range review goal; do
    say "+ $MK --help | grep '^  $v'"
    if [ "$DRY" = 1 ]; then continue; fi
    if "$MK" --help 2>&1 | grep -Eq "^  $v( |$)"; then check "verb-$v" PASS "present: $("$MK" "$v" --help 2>&1 | head -n 1)"
    else check "verb-$v" FAIL "missing in $MK"; missing="$missing $v"; fi
  done
  say "+ $MK status"
  if [ "$DRY" != 1 ]; then "$MK" status 2>&1 | sed 's/^/  | /' | tee -a "$RESULT"; fi
  if [ -n "$missing" ]; then sw_fail "the new binary lacks the verb(s):$missing" "cargo build --manifest-path $REPO/Cargo.toml ; then $MK down --secretary ; then $MK up   (the binary $MK is not this checkout's build)"; fi

  say "== the record that starts the count (Issue 51): $start"
  if [ "$DRY" = 1 ]; then
    plan "write it once: UTC time, origin/main and checkout revisions, the newest event id, the base version of the newest dispatcher_started; when it exists, print it and keep it"
  elif [ -e "$start" ]; then
    check count-start INFO "the count already began; $start is kept as it is:"
    sed 's/^/  | /' "$start" | tee -a "$RESULT"
  else
    local evid bv
    evid=$(sq "SELECT coalesce(max(id), 0) FROM events"); bv=$(sq "SELECT coalesce(json_extract(payload_json, '\$.base_version'), '') FROM events WHERE kind = 'dispatcher_started' ORDER BY id DESC LIMIT 1")
    printf '%s\n' "Issue 51: the count of landed pull requests and of events met begins here." \
      "started_at_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)" "origin_main: $(git -C "$REPO" rev-parse origin/main)" "checkout_head: $(git -C "$REPO" rev-parse HEAD)" \
      "count_after_event_id: $evid" "base_version: $bv" "merge_approval_mode: $mode" > "$start"
    check count-start PASS "written $start (count after event id $evid, base version ${bv:-none})"
    sed 's/^/  | /' "$start" | tee -a "$RESULT"
  fi

  say "== the handover (operator-private: it must never enter this repository)"
  if [ -z "$SWITCH_HANDOVER" ]; then
    say "  no --handover given. Place it yourself with this one line (the file is yours, outside the repository):"
    say "    mkdir -p -m 700 $sec_dir && install -m 600 <your handover file> $hand"
  elif [ "$DRY" = 1 ]; then
    plan "copy $SWITCH_HANDOVER to $hand (mode 0600); an older different one is kept beside it as handover-owner.md.before-<UTC>"
  else
    mkdir -p "$sec_dir"; chmod 700 "$sec_dir"
    if [ -e "$hand" ] && ! [ "$SWITCH_HANDOVER" -ef "$hand" ] && ! cmp -s "$SWITCH_HANDOVER" "$hand"; then
      cp "$hand" "$hand.before-$(date -u +%Y%m%dT%H%M%SZ)"
    fi
    if [ "$SWITCH_HANDOVER" -ef "$hand" ]; then chmod 600 "$hand"; check handover INFO "$hand is the file named: already placed, kept (mode set to 600)"
    else
      install -m 600 "$SWITCH_HANDOVER" "$hand"
      check handover PASS "placed $hand (mode 600, $(wc -c < "$hand") bytes); the original $SWITCH_HANDOVER is untouched"
    fi
  fi

  switch_next_steps "$hand" "$start" "$mode"
}

# What the person does next, printed whole so that nothing is copied by hand but one line at a time.
switch_next_steps() { # switch_next_steps <handover path> <start file> <mode>
  local hand=$1 start=$2 mode=$3 attach issues
  if [ "$DRY" = 1 ]; then attach="HC_SESSION=<session> hc attach secretary"
  else attach=$("$MK" status 2>/dev/null | sed -n 's/^secretary: .*attach with //p'); fi
  [ -n "$attach" ] || attach="(the 'secretary:' line of makiba status prints the attach line)"
  if [ -n "$SWITCH_ISSUES" ]; then issues="the Issues $SWITCH_ISSUES"; else issues="the Issues listed in section 1 of $hand"; fi
  say "== NEXT 1: attach to the secretary (in your login shell), then type the two lines below, one at a time"
  say "    $attach"
  say "== NEXT 2: the first line to type to the secretary (it reads the handover and answers)"
  say "    Read $hand first: it is the handover from the previous organization's secretary (my decisions and how I want to be spoken to). It is private: never copy it into a repository, an issue, a pull request, a document, a message to a worker or a note. Then call status and tell me in three lines where we stand."
  say "== NEXT 3: the second line to type to the secretary (the first range; it proposes and shows what it admits; nothing is dispatched until you approve)"
  say "    Propose the first range for makiba: $issues, 2 at once, 20 tasks in all, the brief head of our usual conventions. Show me what it admits and what it leaves out, with the reason."
  say "== NEXT 4: the merge approval mode in force for makibad: $mode (the first range runs under human; the change to acceptance is the owner's, later)"
  say "    Why human first (the owner's decision of 2026-11-22): on the first real code pull request, see that the agent host shows the landing gate's question to the person (only the human mode can show it; the stage 'first-day' proved the hook, not what Claude Code shows); and judge the secretary's recommendations from the stops on record. After ten recorded stops the owner decides, from how many answers matched the recommendation, whether to switch to acceptance."
  say "    Where it is set: only the environment variable MAKIBA_MERGE_APPROVAL_MODE, read by makibad from its own environment (config set merge_approval_mode is not built; config get always answers null). 'makiba status' and 'makiba range approve' print the value of THEIR shell as 'Merge approval mode'; makibad records its own at approve. The switch to acceptance, when the owner decides it and nothing is in work, is this one line: makiba down --secretary && MAKIBA_MERGE_APPROVAL_MODE=acceptance makiba up   (a range approved under human does not widen: it needs its own approval again)."
  say "== NEXT 5: when the secretary names the range id, approve it in your own shell (only your key can)"
  say "    makiba range approve <id>"
  say "    It prints the draft and makibad's newest read, then asks [y/N]. Check before you type y: 'Merge approval mode' is the one you meant; 'Takes' and the Issues are what you decided; 'At once: 2; tasks in all: 20'; each line '#<n> <title> - admitted' or 'left out: <why>' (read the order; an Issue at a trust boundary should say 'stop after the design' in the Brief head). 'has no complete read yet' means wait; 'stale' means makibad read it again after the preview: run the line again."
  say "== NEXT 6: the count of Issue 51, read afterwards from your shell (the start is in $start)"
  say "    sed -n 's/^count_after_event_id: //p' $start     (the id; call it N)"
  say "    makiba events --after N --json     count the events of kind pr_merged (landed pull requests: twenty) and note when each of these appears: dispatcher_started with a base_version different from the start file (an update of the base), secretary_renewed (a renewal), worker_spawn_failed (a failed launch)"
  say "    makiba status --json     ranges[].items[].place equal to landed shows the same landings per range"
  say "    pr_merged is written only for the tasks of a range (spec 5.8): a pull request landed from a one-off dispatch is not in it; add it by hand, with its number, in docs/reports/self-hosting-week.md"
}

finish() { # the last two lines of the script, so that the switch can end without reading its own file again
  if [ "$FAILS" -gt 0 ]; then say "RESULT self-hosting $STAGE: $FAILS failing check(s); results file ${RESULT:-none}"; exit 1; fi
  say "RESULT self-hosting $STAGE: ok${RESULT:+; results file $RESULT}"
  exit 0
}

stage_cleanup() {
  say "== cleanup: scratch places made by this script (the real organization is never touched)"
  local d found=0
  say "+ ls -d $ORIG_TMP/${SCRATCH_PREFIX}??????"
  for d in "$ORIG_TMP/$SCRATCH_PREFIX"??????; do
    [ -f "$d/.created-by-self-hosting-runbook" ] || continue
    found=1
    say "  found $d"
    if [ "$DRY" = 1 ]; then plan "source $d/env.sh, run: $MK down --secretary --stop-base, then remove $d"; continue; fi
    # shellcheck source=/dev/null
    if ( . "$d/env.sh" && stop_org ); then remove_scratch "$d"
    else check "cleanup-$(basename "$d")" FAIL "makibad of $d still runs, or cannot be shown to be stopped (status gives no 'not running', a pid or a holder of its base session is left, or its env.sh cannot be read); the place is kept: . $d/env.sh; $MK down --secretary --stop-base, then run cleanup again"; fi
  done
  if [ "$found" = 0 ]; then say "  no scratch place left: nothing to do"; fi
  if [ "$DRY" = 1 ]; then plan "report done"
  elif [ "$FAILS" -eq 0 ]; then check cleanup PASS "done; running it again is safe"; fi
}

# ---- go ----

on_exit() {
  local rc=$?
  trap - EXIT
  if [ -n "$BRIEF" ]; then rm -f "$BRIEF"; fi
  if [ -n "$SCRATCH_RUN" ]; then
    scratch_down
    if [ "$rc" -eq 0 ] && [ "$FAILS" -eq 0 ]; then remove_scratch "$SCRATCH_RUN"
    else say "scratch place kept for inspection: $SCRATCH_RUN (the cleanup stage removes it)"; fi
  fi
  if [ "$FAILS" -gt 0 ] && [ "$rc" -eq 0 ]; then rc=1; fi
  fd_summary "$rc"   # first-day only: the last lines of the results file, after the teardown
  exit "$rc"
}
trap on_exit EXIT
trap 'exit 130' INT TERM

COMMIT=$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)
if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no 2>/dev/null)" ]; then COMMIT="$COMMIT+dirty"; fi
open_results
say "stage: $STAGE$([ "$DRY" = 1 ] && echo ' (dry run: commands are printed, none is run)')"
say "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
say "script commit: $COMMIT"

case "$STAGE" in
  check)
    preflight real
    optional_verbs
    ;;
  rehearse)
    stage_rehearse
    ;;
  first-day)
    stage_first_day
    ;;
  register)
    preflight real
    lines_changed "$SLUG's project row and events in the organization's state.db (no verb removes them)
the notices to the secretary, a clone at ${MAKIBA_REPOS_DIR:-$HOME/makiba-repos}/$SLUG, the ledger under ${MAKIBA_GOALS_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/makiba/goals}
the organization is started with makiba up if it does not run (person key and gate key are created if absent)"
    confirm
    stage_register 0
    ;;
  triage)
    preflight real
    lines_changed "one dispatched and closed document task (a real Claude Code worker; trust entries in ~/.claude.json stay)
its out/ folder under ${MAKIBA_DOCS_DIR:-$HOME/makiba-docs}, the run_closed and scan events, the notices to the secretary"
    confirm
    stage_triage 0
    ;;
  switch)
    stage_switch
    finish
    ;;
  cleanup)
    stage_cleanup
    ;;
esac

if [ "$FAILS" -gt 0 ]; then say "RESULT self-hosting $STAGE: $FAILS failing check(s); results file ${RESULT:-none}"; exit 1; fi
say "RESULT self-hosting $STAGE: ok${RESULT:+; results file $RESULT}"
