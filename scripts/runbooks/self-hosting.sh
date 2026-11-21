#!/usr/bin/env bash
# The stages of docs/runbooks/self-hosting.md (Issue 50, M7-1), one line each, typed by the person
# in a login shell on the host (never inside an agent: makiba up refuses that):
#
#   bash scripts/runbooks/self-hosting.sh check       read-only: the preconditions, nothing is changed
#   bash scripts/runbooks/self-hosting.sh rehearse    the whole run in a scratch place; no real place is written
#   bash scripts/runbooks/self-hosting.sh register    REAL: makiba registered in the organization, ledger placed
#   bash scripts/runbooks/self-hosting.sh triage      REAL: one small task dispatched and closed, the scan read
#   bash scripts/runbooks/self-hosting.sh cleanup     removes the scratch places this script made; safe to run twice
#
# Each stage takes --dry-run (prints the commands, runs none, writes no file) and --yes (the real
# stages ask once before they change anything). `register` also takes --replace-ledger.
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
SCRATCH_RUN=""             # the scratch folder of this rehearsal, stopped and (when clean) removed at exit

usage() { sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --yes) ASSUME_YES=1 ;;
    --replace-ledger) REPLACE_LEDGER=1 ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option $a" >&2; usage >&2; exit 2 ;;
    *) if [ -n "$STAGE" ]; then usage >&2; exit 2; fi; STAGE=$a ;;
  esac
done
case "$STAGE" in check | rehearse | register | triage | cleanup) ;; *) usage >&2; exit 2 ;; esac
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
    run cargo build --manifest-path "$REPO/Cargo.toml"
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

# stop_org: ends the organization of this shell's environment (makiba down). 0: it is stopped, or never ran
# here (no state.db); 1: makibad is still alive, or down failed in a place that has run (its terminals or
# base may then remain, and the place is needed to retry).
stop_org() {
  local pid rc=0
  "$MK" down --secretary --stop-base >/dev/null 2>&1 || rc=$?
  if [ ! -f "$(db_path)" ]; then return 0; fi
  pid=$("$MK" status 2>/dev/null | sed -n 's/^makibad: pid \([0-9][0-9]*\)$/\1/p')
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    sleep 3
    if kill -0 "$pid" 2>/dev/null; then return 1; fi
  fi
  [ "$rc" -eq 0 ]
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
    else check "cleanup-$(basename "$d")" FAIL "makibad of $d still runs (or its env.sh cannot be read); the place is kept: . $d/env.sh; $MK down --secretary --stop-base, then run cleanup again"; fi
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
  cleanup)
    stage_cleanup
    ;;
esac

if [ "$FAILS" -gt 0 ]; then say "RESULT self-hosting $STAGE: $FAILS failing check(s); results file ${RESULT:-none}"; exit 1; fi
say "RESULT self-hosting $STAGE: ok${RESULT:+; results file $RESULT}"
