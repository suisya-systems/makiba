# Runbook: makiba in its own organization (M7-1, Issue 50)

This is the written procedure for the three checks of Issue 50: makiba registered as a project of the organization that runs it, its goal ledger placed, and the triage scan run over its open Issues. It is run on a host, by the secretary with the person at the keyboard where a step says so, and the result goes into section 10. The owner closes Issue 50 on that record. Nothing here runs by itself; there is no test for it (the Issue is checked by a person).

Unlike `docs/runbooks/first-worker.md`, this run is meant for the person's real organization: its default places. Section 4 is an optional rehearsal of the same steps in a scratch place first. Whether to rehearse is the owner's choice on the day.

## 1. What this run shows, and what the code cannot do

Shows (Issue 50, definition of done):

1. The project `makiba` is registered (section 6, until the Issue below lands: see the first point of the next list).
2. The triage scan returns candidates from makiba's open Issues (section 8).
3. `tools/check_incidents.py` passes with an empty pending list (section 5, precondition 7).

Where the code at this commit has no way to do what the Issue says, and what this runbook does instead:

- **`makiba status` does not list projects.** It prints the heartbeat, `subscribed` and the secretary's line only (`src/status_local.rs`; `--json` the same). A separate Issue, "`makiba status` lists the registered projects" (test-first), makes it do so. Until that Issue lands, section 6 reads the `projects` row of `state.db` with a read-only query and the results say so. When it has landed, run `makiba status` in section 6 as well and record its project line.
- **No verb runs the triage scan on request.** `makibad` runs it after a close (a `run_closed` event newer than the last scan that did not fail) and at most once per 30 minutes (`MAKIBA_TRIAGE_INTERVAL_MS`, read when `makibad` starts; spec section 10, `src/daemon/triage.rs`). A verb for it is a separate Issue (the owner's decision). Section 8 therefore dispatches one small `document` task in the organization and closes it; the close is what makes the scan due.
- **The scan's answer is a notice to the secretary and an event.** There is no command that prints it. Section 8 reads the event `work_discovery_scanned` read-only; the notice is a row of `messages` from `makibad` to `secretary`.
- **`makiba up` refuses to run inside an agent** (any `CLAUDE*` variable, or `HC_SESSION`, `HC_SOCKET`, `HC_SCOPE`, `HC_TERMINAL_ID`, `HC_TERMINAL_SOCKET`). The secretary is an agent, so the steps marked "the person types" are typed by the person in a login shell.

## 2. What cannot be undone

Read this before section 6. In the default places, these stay after the run, and no verb removes them:

- **The `projects` row of `makiba` and its events.** There is no verb that unregisters a project, and no code sets `projects.status` to anything but `active`. The `project` command's row, the `config_set` event of `allowed_repo_owners`, the `run_closed` of the close, the `work_discovery_scanned` event and every other event of the run stay in `state.db`; `events` is append-only. (`allowed_repo_owners` can be set empty again, which is another event; the first one stays.)
- **The notices delivered to the secretary.** The notice that the project is cloned, the triage notice, and the close's notices are `messages` rows with delivery rows. A delivered notice has been pushed into the secretary's session.
- **The trust entries.** The document task's worker is a real Claude Code launch. Each dialog `makibad` answers adds an entry for the workplace's path to `~/.claude.json`, and Claude Code keeps a transcript folder for that path under `~/.claude/projects/`. Do not edit `~/.claude.json` by hand.

What can be removed by hand afterwards, if the owner wants: the clone `~/makiba-repos/makiba` (only while no worktree of it is in use), the ledger copy, and the document task's `out/` folder.

## 3. The places this run touches

With no variable set, every path is the person's default (spec section 2):

| Place | Default | What the run writes there |
| --- | --- | --- |
| State | `${XDG_STATE_HOME:-~/.local/state}/makiba` (`$MAKIBA_STATE_DIR`) | `state.db` (projects row, commands, events, messages), `makibad.lock`, `makibad.log`, `settings/` |
| Repositories | `~/makiba-repos` (`$MAKIBA_REPOS_DIR`) | the clone `makiba/`, made through `.makiba.partial-<command id>` |
| Goal ledgers | `${XDG_CONFIG_HOME:-~/.config}/makiba/goals` (`$MAKIBA_GOALS_DIR`) | `suisya-systems/makiba.md`, the copy of section 7 |
| Person key, gate key | `${XDG_CONFIG_HOME:-~/.config}/makiba/person.key`, `gate.key` | created by `makiba up` if absent |
| Base configuration | `${XDG_CONFIG_HOME:-~/.config}/makiba/base` | written by `makiba up` |
| Share | `${XDG_DATA_HOME:-~/.local/share}/makiba` (`$MAKIBA_SHARE_DIR`) | the role files and the hooks' entry point |
| Documents | `~/makiba-docs` (`$MAKIBA_DOCS_DIR`) | the task's folder; its `out/` stays after the close |
| Door | `${XDG_RUNTIME_DIR}/makiba/door.sock` (`$MAKIBA_DOOR`) | the person's socket |
| Base session | `makiba` (`$MAKIBA_BASE_SESSION`) | the core and its terminals |
| Claude Code | `~/.claude.json`, `~/.claude/projects/` | trust entries and transcripts (section 2) |

GitHub is read, not written: `git clone` / `git fetch` of `suisya-systems/makiba`, and `gh issue list` / `gh pr list` of it.

## 4. Optional: a rehearsal in a scratch place

The same steps (sections 6 to 9) can be run first in a scratch place, so that nothing of section 3 is written. It reads the same GitHub repository and launches the same kind of worker, so it shows the procedure works on this host; it does not register makiba in the real organization, and the real run (sections 5 to 9 with no variable set) is still needed afterwards. It leaves trust entries in `~/.claude.json` for the scratch path, like the real run.

In a login shell, in the repository (with `MK` set, section 5, precondition 6), set the variables of `docs/runbooks/first-worker.md` section 3: run its first block with `makiba-m71` in place of `makiba-m45` in `RUN` and `SESSION`, and take the snapshot of the default places there (`snap > "$RUN/defaults-before.txt"`). Before that block, keep the GitHub CLI's own configuration where it is: the block moves `XDG_CONFIG_HOME` to an empty folder, and the GitHub CLI reads its stored login and `git_protocol` from `$XDG_CONFIG_HOME/gh` unless `GH_CONFIG_DIR` is set. `makibad` keeps `GH_CONFIG_DIR` (only the agents' launches empty it, spec section 6.4), so set it first and add it to `env.sh` after the block:

```sh
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/gh}"
gh auth status -h github.com
```

and after the block, `echo "export GH_CONFIG_DIR='$GH_CONFIG_DIR'" >> "$RUN/env.sh"`. Skip its scratch repository block: here `makibad` clones the project itself. Every step below reads its places through the same variables (`${MAKIBA_STATE_DIR:-...}`, `${XDG_CONFIG_HOME:-...}`, `${MAKIBA_REPOS_DIR:-...}`), so they run unchanged. Clean up as first-worker.md section 8 says (its `hcs kill "$W"` line has nothing left to end: the close of section 8 here ended the task's terminal), and check that the snapshot's `diff` is empty.

## 5. Preconditions

Check each one; stop at the first that fails and say which.

1. **A person's login shell, on the host, not inside an agent.** `env | grep -E '^(CLAUDE|HC_)'` prints nothing.
2. **The versions and programs of `docs/runbooks/first-worker.md` section 2**, items 3 to 5: `hc`, `claude` (logged in), `git`, `python3` with its `sqlite3` module, `gh`.
3. **The GitHub CLI is logged in to github.com with a stored login.** `gh auth status -h github.com` says logged in. `makiba up` removes `GH_TOKEN`, `GITHUB_TOKEN` and their enterprise forms from `makibad`'s environment (spec section 2), so a login held only in such a variable does not reach the clone or the scan. `gh config get git_protocol -h github.com` decides the clone's URL: `ssh` gives `git@github.com:suisya-systems/makiba.git` (an ssh key usable with `BatchMode=yes`, no passphrase prompt), anything else `https://github.com/suisya-systems/makiba.git` (through `gh auth git-credential`).
4. **No organization runs on the default places yet, or the one that runs is this one.** `"$MK" status` either says `makibad has never run here` or shows the organization the owner means to use.
5. **The repository folder.** `ls ~/makiba-repos/makiba` (or `$MAKIBA_REPOS_DIR/makiba`). If it does not exist, `makibad` clones it. If it exists, its `origin` must be `suisya-systems/makiba`, it must be on its default branch and have no uncommitted change to tracked files: `makibad` fetches it and fast-forwards that branch, else the command fails with `conflict`. Check with `git -C ~/makiba-repos/makiba remote get-url origin; git -C ~/makiba-repos/makiba status --short --branch --untracked-files=no`.
6. **A build of this commit:** `cargo build` in the repository, then `MK=$PWD/target/debug/makiba`, `REPO=$PWD` and `COMMIT=$(git rev-parse --short HEAD)`. Write `COMMIT`, `hc --version`, `claude --version` and `gh --version` in the results.
7. **The pending list is empty and the check passes** (definition of done, item 3). In the repository at `COMMIT`:

   ```sh
   grep -v '^#' docs/design/incidents-pending.txt | grep -c .
   python3 tools/check_incidents.py; echo "exit=$?"
   ```

   Look at: `0`, then `incident table ok` and `exit=0`.

The helper of the later sections, in the same shell:

```sh
DB="${MAKIBA_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/makiba}/state.db"
sq() { python3 -I -c 'import sqlite3, sys
c = sqlite3.connect("file:" + sys.argv[1] + "?mode=ro", uri=True)
for r in c.execute(sys.argv[2]):
    print("|".join("" if v is None else str(v) for v in r))' "$DB" "$@"; }
```

`sq "<query>"` opens `state.db` read-only and prints one row per line, the columns joined by `|`.

## 6. Register makiba (the person types this)

Start the organization if it is not running. `makiba up` starts the base, `makibad` and the secretary; `--no-secretary` leaves the secretary out (the notices then wait in the ledger, and every check below is read from `state.db` either way). Which one is the owner's choice.

```sh
"$MK" up
"$MK" status; echo "exit=$?"
```

Look at: `makibad: started` (or that it already runs), then `status` with a heartbeat a few seconds old, `subscribed: yes`, exit 0. When `up` fails, use the table of first-worker.md section 3.

Allow the organization that owns makiba, so that the secretary may fetch the project again later (the person may register any repository; the secretary only one whose owner is listed, `src/daemon/project.rs`):

```sh
"$MK" config set allowed_repo_owners suisya-systems
```

Look at: `{"key":"allowed_repo_owners","project":"","value":"suisya-systems"}`. If the organization already lists other owners, set the comma list of all of them (`"$MK" config get allowed_repo_owners` shows the current one); `set` replaces the value.

Register the project. The request is `OWNER/NAME` only; `makibad` builds the URL:

```sh
"$MK" project makiba suisya-systems/makiba
```

Look at: `{"repo":"suisya-systems/makiba","slug":"makiba","status":"accepted"}`, exit 0. The clone runs on a thread of `makibad` and can take minutes (limit 30 minutes). Wait for the command to finish:

```sh
sq "SELECT id, status, error_kind, result_json FROM commands WHERE verb = 'project' ORDER BY id DESC LIMIT 1"
```

Repeat until `status` is `done` (or `failed`). Look at: `result_json` with `"action":"clone"` (or `"fetch"` when the folder existed), `"default_branch":"main"` and a `head`. Then the check of the definition of done, item 1:

```sh
sq "SELECT slug, origin_url, default_branch, status FROM projects WHERE slug = 'makiba'"
sq "SELECT body FROM messages WHERE sender = 'makibad' AND body LIKE 'Project makiba %' ORDER BY id DESC LIMIT 1"
git -C "${MAKIBA_REPOS_DIR:-$HOME/makiba-repos}/makiba" log --oneline -1
```

Look at: `makiba|https://github.com/suisya-systems/makiba|main|active`; the notice `Project makiba (suisya-systems/makiba) is cloned: main is at <sha>. You may dispatch into it.`; the clone's newest commit is that `<sha>`. Until the Issue "`makiba status` lists the registered projects" lands, this query is the check, and the results say so; after it lands, `"$MK" status` shows the project and that line is recorded too.

If it differs:

| What you see | Meaning | Do |
| --- | --- | --- |
| exit 4 `denied: the owner ... is not in the organization's allowed_repo_owners` | the request did not come as the person | run it in the person's shell, with the person key |
| exit 5 `conflict: project makiba is the repository ..., not ...` | the slug is registered to another repository | stop; the owner decides (a slug names one repository) |
| `failed`, `... is a clone of ..., not of suisya-systems/makiba` | precondition 5 | stop; the folder is left as it is |
| `failed`, `... is on <branch>, not on the default branch` or `has uncommitted changes` | precondition 5 | put the folder on `main` and clean, then run `project` again |
| `failed`, `git clone failed: ...` | git's own words: login, network, ssh | check precondition 3; `tail "${MAKIBA_STATE_DIR:-$HOME/.local/state/makiba}/makibad.log"` |

## 7. Place the goal ledger (the person types this)

The ledger is a file of the operator, outside every repository (`docs/goals.md`). makiba's own ledger is kept in this repository at `goals/suisya-systems/makiba.md`; the scan reads the copy in the configuration folder, not the repository's.

```sh
GOALS="${MAKIBA_GOALS_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/makiba/goals}"
mkdir -p "$GOALS/suisya-systems"
cp -i "$REPO/goals/suisya-systems/makiba.md" "$GOALS/suisya-systems/makiba.md"
grep -c '^## G[0-9]' "$GOALS/suisya-systems/makiba.md"
```

Look at: the number of clauses (`## G1` ... `## G<n>`) of the repository's ledger. `cp -i` asks before replacing a ledger that is already there; if one is, compare first (below) and answer `n` unless the owner wants it replaced.

The copy is the operator's: it does not follow the repository. When `goals/` changes on `main`, the copy is not updated by anything. Compare the two at any time with:

```sh
diff "$REPO/goals/suisya-systems/makiba.md" "$GOALS/suisya-systems/makiba.md" && echo "the ledger copy matches the repository"
```

The scan reads the ledger at each scan; nothing needs restarting after a change. `MAKIBA_GOALS_DIR` is read by `makibad` when it starts: if it is set in the person's shell, it must be the same in the shell that ran `makiba up`.

## 8. Run the triage scan (the person types the dispatch and the close)

The scan runs after a close. Find whether one has run in the last 30 minutes:

```sh
sq "SELECT id, occurred_at, json_extract(payload_json, '$.status') FROM events WHERE kind = 'work_discovery_scanned' ORDER BY id DESC LIMIT 1"
sq "SELECT (julianday('now') - julianday(max(occurred_at))) * 1440 FROM events WHERE kind = 'work_discovery_scanned'"
```

The second line is the age of the newest scan in minutes (empty: none ever ran). If it is below 30, the close below is still recorded, and the scan runs by itself once the 30 minutes from that scan have passed (a close inside the interval waits for its end); wait for that rather than closing more tasks. A scan made before the project was registered does not count: the close below is newer than it.

One small document task. Its folder is `~/makiba-docs/m71-triage` (or `$MAKIBA_DOCS_DIR/m71-triage`):

```sh
BRIEF=$(mktemp)
cat > "$BRIEF" <<'EOF'
Write the file out/note.md with the single line "triage trigger for M7-1". When it is written, call the tool report with state "done", task "m71-triage" and one sentence that says what you wrote.
EOF
"$MK" dispatch --kind document --task-id m71-triage --title "Triage trigger (M7-1)" --brief-file "$BRIEF"
```

Look at: `{"spawn_id":"...","status":"queued","task_id":"m71-triage"}`, exit 0 (exit 5 `conflict`: the task id was used; choose another and use it below). Wait for the done report:

```sh
sq "SELECT r.status FROM runs r WHERE r.task_id = 'm71-triage'"
sq "SELECT id, substr(body, 1, 200) FROM messages WHERE kind = 'report_done' AND run_id = (SELECT id FROM runs WHERE task_id = 'm71-triage')"
```

Repeat until the run is `review` and the report is there. Then close it:

```sh
"$MK" close m71-triage; echo "exit=$?"
sq "SELECT id, kind FROM events WHERE kind = 'run_closed' ORDER BY id DESC LIMIT 1"
```

Look at: exit 0 and a `run_closed` event. If the worker does not report (a stop, a dialog it cannot pass), `"$MK" close m71-triage --abandon` writes `run_closed` as well; record why. The close ends the worker's terminal and removes the task's folder except `out/`.

Now wait for the scan (seconds to a minute: it reads GitHub on a thread of its own):

```sh
sq "SELECT id, json_extract(payload_json, '$.status'), json_extract(payload_json, '$.repos'), json_extract(payload_json, '$.candidate_count'), json_extract(payload_json, '$.candidate_refs'), json_extract(payload_json, '$.diagnostics'), json_extract(payload_json, '$.error') FROM events WHERE kind = 'work_discovery_scanned' ORDER BY id DESC LIMIT 1"
```

Look at, for the definition of done, item 2: an event newer than the `run_closed` above, with status `candidates_found`, `repos` `["suisya-systems/makiba"]` (and any other active project), a `candidate_count` above 0, and `candidate_refs` a comma list of `suisya-systems/makiba#<number>`, each an open Issue of makiba (check two of them with `gh issue view <number> --repo suisya-systems/makiba`). `diagnostics` does not hold `unset:suisya-systems/makiba` or `error:suisya-systems/makiba`. Then the notice:

```sh
sq "SELECT id, body FROM messages WHERE sender = 'makibad' AND body LIKE 'Triage after a close:%' ORDER BY id DESC LIMIT 1"
```

Look at: `Triage after a close: <n> candidates (free workers: <k>).`, then lines `1. suisya-systems/makiba#<number> <title> - G<i> <heading>` (the clause the Issue names under its `## Goal clause` heading; `names no clause` when it names none), and `Clauses of suisya-systems/makiba: G1 ...; G2 ...`. A notice is written only when the candidates or diagnostics differ from the last notified scan; the event is written either way.

If it differs:

| What you see | Meaning | Do |
| --- | --- | --- |
| no new event after some minutes | a scan ran less than 30 minutes before, or makibad is not running | the age query above; `"$MK" status` |
| status `error`, `error` `gh not found` or `gh` text | the GitHub CLI cannot be run or read | precondition 3; the close stays due and the next pass after the interval tries again |
| `diagnostics` `unset:suisya-systems/makiba` | no ledger at the place makibad reads | section 7; compare `MAKIBA_GOALS_DIR` in the shell that ran `up` |
| `diagnostics` `error:suisya-systems/makiba` | the ledger cannot be read | the notice gives the line and what is wrong; fix the copy, not the repository |
| `diagnostics` `norepo:makiba` | the project row has no URL and the clone no single `origin` | section 6 |
| status `no_candidates` | the scan read makiba and proposed nothing | record it with the open Issues of the moment (`gh issue list --repo suisya-systems/makiba`); the owner decides whether item 2 holds |

## 9. Cleanup

Nothing of the real run is cleaned: the project, the clone and the ledger are what the run was for, and section 2 lists what stays. Check that the document task left nothing running:

```sh
sq "SELECT status FROM runs WHERE task_id = 'm71-triage'"
sq "SELECT status FROM commands WHERE verb = 'close' ORDER BY id DESC LIMIT 1"
ls "${MAKIBA_DOCS_DIR:-$HOME/makiba-docs}/m71-triage"
```

Look at: `completed` (or `abandoned`), the close `done`, and only `out` in the folder. `rm` of the `out/` folder is the owner's choice. Leave the organization running or stop it with `"$MK" down`, as the owner wants.

For the rehearsal of section 4, clean up as first-worker.md section 8 instead.

## 10. Results (filled in by the secretary)

One row per check; "what happened" is one line with the exact text that showed it; an Issue number for every surprise. Nothing specific to the host goes here: no home paths, no account names. Write a placeholder such as `<host>` and say what it stands for.

### 10.1 The run

Leave this empty until the run.

| Item | Value |
| --- | --- |
| Date and time (UTC) | |
| Host | |
| `makiba` commit (`$COMMIT`) | |
| Runbook commit | |
| `hc --version`, `claude --version`, `gh --version` | |
| Rehearsal in a scratch place first (section 4): yes / no | |
| `makiba up` with or without the secretary | |
| Who typed sections 6 to 8 (the person) | |

| Check | Pass / fail / not run | What happened | Issue |
| --- | --- | --- | --- |
| Pending list empty, `check_incidents.py` exit 0 (precondition 7, DoD 3) | | | |
| `config set allowed_repo_owners` | | | |
| `project makiba suisya-systems/makiba` accepted, command `done` (`action`) | | | |
| `projects` row (query of section 6, DoD 1 until the status Issue lands) | | | |
| `makiba status` shows the project (after the status Issue lands) | | | |
| Clone at the notice's head | | | |
| Ledger copied; `diff` with the repository | | | |
| Document task: dispatch, done report, `close`, `run_closed` | | | |
| `work_discovery_scanned`: status, `candidate_count`, `candidate_refs`, `diagnostics` (DoD 2) | | | |
| Two candidates checked as open Issues of makiba | | | |
| Triage notice (first lines) | | | |
| Leftovers (section 2: trust entries, `out/`) | | | |
| Permission prompts seen | | | |
| Issues filed for surprises | | | |

Owner's decision on Issue 50 (after reading 10.1): closed / not closed, with the reason.
