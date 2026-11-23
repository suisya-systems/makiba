# Runbook: makiba in its own organization (M7-1, Issue 50)

This is the procedure for the three checks of Issue 50: makiba registered as a project of the organization that runs it, its goal ledger placed, and the triage scan run over its open Issues. It is run on the host, in the person's login shell, one line per stage. The result goes into section 9; the owner closes Issue 50 on that record. There is no test for it (the Issue is checked by a person).

The person types one short line per stage (section 3) and reads what they print. Everything long, every wait and every check lives in `scripts/runbooks/self-hosting.sh`, which prints what it is about to do, refuses to go on when a precondition is missing (and says how to fix it), and writes a results file the secretary reads. It reads `state.db` read-only, never edits `~/.claude.json`, and deletes only what it created.

## 1. What cannot be undone (read this before `register` and `triage`)

`rehearse` writes none of this in the person's real places. `register` and `triage` do. Afterwards, in the default places, these stay and no verb removes them:

- **The `projects` row of `makiba` and its events.** There is no verb that unregisters a project, and no code sets `projects.status` to anything but `active`. The `project` command's row, the `config_set` event of `allowed_repo_owners`, the `run_closed` of the close, the `work_discovery_scanned` event and every other event of the run stay in `state.db`; `events` is append-only. (`allowed_repo_owners` and `network_hosts` can be set again, which is another event; the first one stays.)
- **The notices delivered to the secretary.** The notice that the project is cloned, the triage notice and the close's notices are `messages` rows with delivery rows. A delivered notice has been pushed into the secretary's session.
- **The trust entries.** The document task's worker (and the secretary, when `up` starts it) is a real Claude Code launch. Each dialog `makibad` answers adds an entry for the workplace's path to `~/.claude.json`, and Claude Code keeps a transcript folder for that path under `~/.claude/projects/`. The rehearsal leaves such entries too, for its scratch path. Do not edit `~/.claude.json` by hand.
- **The network list of the project.** Workers of makiba reach only the hosts listed for it (section 5); that list is a setting with its own event.

What can be removed by hand afterwards, if the owner wants: the clone `~/makiba-repos/makiba` (only while no worktree of it is in use), the ledger copy, and the document task's `out/` folder. The scripts remove none of them.

`register` and `triage` ask once, print what they are about to change, and need a typed `yes` (or `--yes`).

## 2. What this run shows, and what the code still cannot do

Shows (Issue 50, definition of done):

1. The project `makiba` is registered: `makiba status` lists it (`projects:` section). Issue 227 landed; the old read of the `projects` row is no longer needed.
2. The triage scan returns candidates from makiba's open Issues (the `work_discovery_scanned` event, and `next work:` in `makiba status`).
3. `tools/check_incidents.py` passes with an empty pending list (a precondition of every stage).

Where the code at this commit has no way to do what the Issue says:

- **No verb runs the triage scan on request.** `makibad` runs it after a close (a `run_closed` event newer than the last scan that did not fail) and at most once per 30 minutes (`MAKIBA_TRIAGE_INTERVAL_MS`, read when `makibad` starts; spec section 10, `src/daemon/triage.rs`). So `triage` dispatches one small `document` task in the organization and closes it; the close is what makes the scan due. If a scan ran less than 30 minutes before, the script says so and waits for the interval to pass.
- **The scan's answer is an event and a notice.** `triage` reads the event `work_discovery_scanned` read-only; the notice is a row of `messages` from `makibad` to `secretary`. `makiba status` also prints `next work:` (the candidate count, the recommended one, or why there is none).
- **`makiba up` refuses to run inside an agent** (any `CLAUDE*` variable, or `HC_SESSION`, `HC_SOCKET`, `HC_SCOPE`, `HC_TERMINAL_ID`, `HC_TERMINAL_SOCKET`; `HC_BIN`, which names the `hc` program, is allowed). The secretary is an agent, so every stage is typed by the person in a login shell, and each script refuses at once when it finds such a variable.
- **The goal ledger is the operator's copy.** The scan reads `<goals dir>/suisya-systems/makiba.md`, not the repository's file. The ledger of the repository holds the clauses of Issue 50's `## Goal clause` list (`issues/M7-1.md`); the script prints how many clauses the placed ledger has and refuses when it lacks the clause the Issue names, so a ledger that has not landed shows before the scan and not as "no candidate".
- **Two read surfaces are not in every build.** A `range` verb (pull request 257) and a `review` verb (pull request 261) may not have landed. The scripts look for each in `makiba --help`, print what a present one adds and print `skipped` for an absent one; the runbook uses neither, and nothing is approved or reviewed by it.

## 3. The lines to type

In a login shell on the host (not inside an agent), in the repository, at the commit the owner means to run:

| Stage | The line | What it does | Real places written |
| --- | --- | --- | --- |
| Check | `bash scripts/runbooks/self-hosting.sh check` | The preconditions of section 4, an incremental `cargo build` of this commit (unless `MK=<path>` names a binary), the ledger clause count. Changes nothing. | none (a results file in `.runbook-results/`) |
| Rehearsal | `bash scripts/runbooks/self-hosting.sh rehearse` | Everything below, `register` then `triage`, in a scratch place that has its own state, person key, door, port, base session, clone and ledger. It stops its own organization at the end and removes the scratch folder when all went well. | none, but the trust entries of section 1 |
| First day | `bash scripts/runbooks/self-hosting.sh first-day` | The four checks before makiba's first range (section 10), in a scratch place like the rehearsal's: the build of this checkout, one real code worker that builds and asks for a diff review, the review by each reader, the landing gate's question. | none, but the trust entries of section 1 |
| Register | `bash scripts/runbooks/self-hosting.sh register` | Starts the organization if it does not run, allows the owner `suisya-systems`, registers `makiba` with its network hosts, waits for the clone, places the ledger. | all of section 1 |
| Triage | `bash scripts/runbooks/self-hosting.sh triage` | Dispatches one small document task, waits for its done report, closes it, waits for the scan, prints the scan's answer, checks two candidates with the GitHub CLI. | the events, notices and `out/` folder of section 1 |
| Switch | `bash scripts/runbooks/self-hosting.sh switch --handover <file> [--issues 169,270]` | Restarts the real organization on `origin/main` with a new secretary, places the owner's private handover, and starts the count of Issue 51 (section 11). | all of section 11 |
| Cleanup | `bash scripts/runbooks/self-hosting.sh cleanup` | Stops and removes the scratch places this script made (found by a marker file it wrote). Safe to run twice. It never touches the real organization. | none |

Each takes `--dry-run`: it prints every command and runs none, and writes no file. Run that first if you want to see what a stage will do. `register` and `triage` also take `--yes`, which skips the question. `register --replace-ledger` replaces a different ledger already placed (the old one is kept beside it as `makiba.md.before-<UTC>`; without the flag a different ledger is kept and only the clause check applies).

The order of the day: `check`, then `rehearse` (optional; whether to rehearse is the owner's choice on the day), then `register`, then `triage`. `first-day` (section 10) is its own line, run once before the owner moves makiba's development onto makiba (Issue 51); it needs none of the others. `switch` (section 11) is the move itself, after `first-day`. Each prints `RESULT self-hosting <stage>: ok` and the path of its results file, or `REFUSED: ... fix: ...`. A refusal changed nothing in that step; fix what it names and run the same line again. `register` and `triage` are safe to run again: an organization that runs is not started again, a project already registered with the same hosts is not registered again, and a ledger already placed is not replaced.

The results files are `.runbook-results/self-hosting-<stage>-<UTC>.txt` (not committed; `RUNBOOK_RESULTS_DIR` moves them). Every `CHECK <name>: PASS|FAIL|INFO` line there is a row of section 9. After a rehearsal the events and messages of the scratch organization are kept beside its file as `-events.txt` and `-messages.txt`.

### What only the person can do

The scripts need the person key, so the person types the lines; that is the one named step. Besides it:

- **Typing `yes`** at the question of `register` and `triage` (section 1 lists what it covers).
- **A dialog `makibad` cannot pass.** `makibad` answers the folder-trust dialog and the development-channel dialog itself (`DIALOGS` in `src/agents/claude_code.rs`; probes.md P10), for the secretary and for the document worker; the person approves nothing by hand. A dialog that is not on that list makes the launch fail with the screen recorded: the script then stops and tells the person to read the worker's screen, and to end the task with `makiba close <task> --abandon` (the exact line is printed; write why in the results).
- **`gh auth login -h github.com`**, if the check says there is no stored login.

## 4. Preconditions (the script checks each; the first that fails stops it and says how to fix it)

1. A login shell on the host, not inside an agent.
2. On PATH: `hc`, `claude` (logged in; open it once by hand if unsure), `git` 2.38 or newer, `python3` with its `sqlite3` module and the JSON functions, `gh`, `cargo`. Versions of `hc`, `claude`, `gh` and `git` go into the results file; so does the commit.
3. The GitHub CLI has a stored login for github.com. `makiba up` removes `GH_TOKEN`, `GITHUB_TOKEN` and their enterprise forms from `makibad`'s environment (spec section 2), so a login held only in such a variable does not reach the clone or the scan. `gh config get git_protocol -h github.com` decides the clone's URL: `ssh` (a key usable with `BatchMode=yes`) or `https` (through `gh auth git-credential`). The rehearsal keeps `GH_CONFIG_DIR` where it is, since the CLI would otherwise look for its login under the scratch `XDG_CONFIG_HOME`.
4. The pending list `docs/design/incidents-pending.txt` is empty and `tools/check_incidents.py` passes (definition of done 3).
5. The ledger of the repository has the clause the Issue names (`issues/M7-1.md`, `## Goal clause`).
6. For `register` and `triage`: no test seam is set (`MAKIBA_BASE`, `MAKIBA_LAUNCH_AGENT`, `MAKIBA_FAKE_AGENT`, `MAKIBA_DOOR=none`), and the project's folder `~/makiba-repos/makiba` (`$MAKIBA_REPOS_DIR/makiba`) is either absent (`makibad` clones it) or a clone of `suisya-systems/makiba`, on its default branch, with no uncommitted change to tracked files (`makibad` fetches it and fast-forwards that branch, else the command fails with `conflict`).
7. A code run is not part of this runbook (the dispatched task is a `document` task), so the repository a code run keeps of its own (`<state>/git/<task>`, Issue 241) and the project's own git hooks (Issue 236) are not exercised here; a later `code` dispatch into makiba needs the network list of section 5, and `git` 2.38 or newer for `update`.

## 5. The network list of makiba's workers

A worker's and the curator's settings turn Claude Code's strict allow-list on: a worker reaches only the hosts listed for its project, none when none is listed, and a host outside the list is refused in every permission mode, with no prompt (`docs/design/lines.md` L-K2 and L-K7; Issue 244; `tests/network_allowlist.rs`). GitHub's hosts are never accepted into a list: pushes and GitHub writes go through `makibad`. Only the person sets the list.

`register` registers makiba with `--hosts index.crates.io,static.crates.io`, the pair of hosts cargo's sparse registry uses for its index and its downloads, so that a later `code` worker of makiba can `cargo build`. It prints the list before it registers. `MAKIBA_RUNBOOK_HOSTS=a.example,b.example` changes it for that run.

**Only the host run proves the list is enough.** The pair is from cargo's behaviour, not from a document or a test of this repository (the one test that names a crates host uses `static.crates.io` alone, as a value). The document task of `triage` reaches no host, so it does not show it either. When a code worker's `cargo build` fails to fetch a crate, the refusal is a connection or proxy error for a named host, with no permission prompt; the person or the secretary adds the host, for example:

```sh
makiba config set network_hosts --project makiba index.crates.io,static.crates.io,<the missing host>
```

`config set` replaces the whole list, so name every host. `makiba status` shows the list of each project (`network hosts:`). This is a setting with its own event (section 1).

## 6. The places a real run touches

With no variable set, every path is the person's default (spec section 2):

| Place | Default | What the run writes there |
| --- | --- | --- |
| State | `${XDG_STATE_HOME:-~/.local/state}/makiba` (`$MAKIBA_STATE_DIR`) | `state.db` (projects row, commands, events, messages), `makibad.lock`, `makibad.log`, `settings/` |
| Repositories | `~/makiba-repos` (`$MAKIBA_REPOS_DIR`) | the clone `makiba/`, made through `.makiba.partial-<command id>` |
| Goal ledgers | `${XDG_CONFIG_HOME:-~/.config}/makiba/goals` (`$MAKIBA_GOALS_DIR`) | `suisya-systems/makiba.md`, the copy of the repository's ledger (never replaced without `--replace-ledger`) |
| Person key, gate key | `${XDG_CONFIG_HOME:-~/.config}/makiba/person.key`, `gate.key` | created by `makiba up` if absent |
| Base configuration | `${XDG_CONFIG_HOME:-~/.config}/makiba/base` | written by `makiba up` |
| Share | `${XDG_DATA_HOME:-~/.local/share}/makiba` (`$MAKIBA_SHARE_DIR`) | the role files and the hooks' entry point |
| Documents | `~/makiba-docs` (`$MAKIBA_DOCS_DIR`) | the task's folder; its `out/` stays after the close |
| Door | `${XDG_RUNTIME_DIR}/makiba/door.sock` (`$MAKIBA_DOOR`) | the person's socket |
| Base session | `makiba` (`$MAKIBA_BASE_SESSION`) | the core and its terminals |
| Claude Code | `~/.claude.json`, `~/.claude/projects/` | trust entries and transcripts (section 1) |
| The repository | `.runbook-results/` | the results files of the scripts |

GitHub is read, not written: `git clone` / `git fetch` of `suisya-systems/makiba`, and the scan's reads of its Issues and pull requests, and `gh issue view` of two candidates.

The rehearsal moves every one of these into `${TMPDIR:-/tmp}/makiba-rehearsal-XXXXXX` (its `env.sh` holds the variables), and uses its own base session and port. The scratch `XDG_CONFIG_HOME` also hides a global git configuration kept under `~/.config/git`; the clone uses the person's ssh key or the CLI's login, not that configuration.

## 7. If a stage refuses or a check fails

| What you see | Meaning | Do |
| --- | --- | --- |
| `REFUSED: a CLAUDE* or HC_SESSION/HC_SOCKET/HC_SCOPE/HC_TERMINAL_* variable is set` | this shell is inside an agent or a terminal of the base | open a plain login shell; do not unset the variable by hand |
| `makiba: unavailable: Claude Code version ...` (from `up`) | the version of `claude` is outside the supported range | stop; the owner decides (`docs/upgrading-claude-code.md`) |
| other failures of `up` | see the table of `docs/runbooks/first-worker.md` section 3 | read `makibad.log` in the state folder |
| `REFUSED: the ... ledger has no clause G<n>` | the ledger does not hold the clause Issue 50 names (the clause list of the repository has not landed, pull request 260 at the time of writing) | land it, pull main, then `register --replace-ledger` |
| `conflict: project makiba is the repository ..., not ...` | the slug is registered to another repository | stop; the owner decides (a slug names one repository) |
| the `project` command `failed`, `... is a clone of ..., not of suisya-systems/makiba` | precondition 6 | stop; the folder is left as it is |
| `failed`, `... is on <branch>, not on the default branch` or `has uncommitted changes` | precondition 6 | put the folder on `main` and clean, then run `register` again |
| `failed`, `git clone failed: ...` | git's own words: login, network, ssh | precondition 3; `tail` of `makibad.log` |
| the worker did not report (a stop, a dialog it cannot pass) | see "What only the person can do" | read its screen; `makiba close <task> --abandon` as printed |
| no scan after the close | a scan ran less than 30 minutes before, or `makibad` is not running | the `last-scan` line says how old the last scan was; `makiba status` |
| scan status `error` with `gh not found` or GitHub CLI text | the CLI cannot be run or read | precondition 3; the close stays due and the next pass after the interval tries again |
| `diagnostics` holds `unset:suisya-systems/makiba` | no ledger at the place `makibad` reads | `MAKIBA_GOALS_DIR` is read when `makibad` starts: it must be the same in the shell that ran `makiba up` |
| `diagnostics` holds `error:suisya-systems/makiba` | the ledger cannot be read | the notice gives the line and what is wrong; fix the copy, not the repository |
| `diagnostics` holds `norepo:makiba` | the project row has no URL and the clone no single `origin` | run `register` again |
| scan status `no_candidates` | the scan read makiba and proposed nothing | `makiba status` prints `next work: ... why: ...`; record it with the open Issues of the moment; the owner decides whether item 2 holds |

## 8. Cleanup

Nothing of the real run is cleaned: the project, the clone and the ledger are what the run was for, and section 1 lists what stays. Leave the organization running or stop it with `makiba down` (add `--stop-base` to stop the core), as the owner wants. After a rehearsal that failed, its scratch place is kept for inspection and its organization is stopped (if `makiba down` fails in a place that has run, or `makibad` stays alive, the place is kept, the stage fails and the line to stop it by hand is printed); `bash scripts/runbooks/self-hosting.sh cleanup` removes it.

## 9. Results (filled in by the secretary)

One row per check; "what happened" is one line with the exact text that showed it; an Issue number for every surprise. Nothing specific to the host goes here: no home paths, no account names. Write a placeholder such as `<host>` and say what it stands for. The `CHECK` lines of the results files carry most of it.

### 9.1 The run

Leave this empty until the run.

| Item | Value |
| --- | --- |
| Date and time (UTC) | |
| Host | |
| `makiba` commit (the `commit` line) | |
| Runbook and scripts commit | |
| `hc`, `claude`, `gh`, `git` versions (the `versions` line) | |
| Rehearsal first: yes / no | |
| `makiba up` with or without the secretary | |
| Who typed the lines (the person) | |

| Check | Pass / fail / not run | What happened | Issue |
| --- | --- | --- | --- |
| Pending list empty, `check_incidents.py` passes (DoD 3) | | | |
| Ledger of the repository: clause count, clause named by the Issue (`ledger-repository`) | | | |
| `allowed_repo_owners` set | | | |
| `project makiba suisya-systems/makiba --hosts ...` accepted, command done (`action`) | | | |
| `makiba status` lists the project, with its hosts (DoD 1; `status-lists-project`) | | | |
| Ledger placed; clause count and clause (`ledger-placed`) | | | |
| Document task: dispatch, done report, `close` | | | |
| `work_discovery_scanned`: status, `candidate_count`, `candidate_refs`, `diagnostics` (DoD 2) | | | |
| Two candidates checked as open Issues of makiba | | | |
| Triage notice (first lines), and `next work:` in `makiba status` | | | |
| `range` and `review` verbs: present or skipped (`verb-range`, `verb-review`) | | | |
| Crates hosts enough for a code worker's `cargo build` (not run by this runbook: write "not run", or what a later code run showed) | | | |
| Leftovers (section 1: trust entries, `out/`) | | | |
| Permission prompts seen | | | |
| Issues filed for surprises | | | |

Owner's decision on Issue 50 (after reading 9.1): closed / not closed, with the reason.

## 10. The first-day checks (stage `first-day`, Issue 51)

The owner moves makiba's development onto makiba itself once four things are known to hold on the real host. No test can show them (CI has no host, no `claude`, no reader, no network list), so this stage shows what a script can and names what it cannot. It is a stage of `self-hosting.sh`, not a second script: `scratch_env`, `stop_org`, the marker file, `stage_register` (the network hosts of section 5) and the exit handler that stops and removes the scratch place live there, and a second script would copy them. The person's line is

```sh
bash scripts/runbooks/self-hosting.sh first-day
```

in a login shell on the host, in the repository, at the commit the owner means to run. `--dry-run` prints every command and writes nothing. It never needs `--yes`: it asks nothing, because it changes nothing real. Expect **25 to 60 minutes**, most of it the first clone (up to 30 minutes, `WAIT_CLONE`) and the worker's cold `cargo fetch` and build (up to 50 minutes, `WAIT_CODE`; the diff review inside it is bounded at 20 minutes). Each wait prints its own limit when it starts. The cost is one real code worker (about 10 to 25 turns on a tiny task) and one diff review, and a second worker and review only when the first review was read by the other model's program.

It writes `.runbook-results/self-hosting-first-day-<UTC>.txt`, which ends with one `SUMMARY` line per check, a `NEXT` line for each `FAIL`, a `NOT SHOWN` line for each thing no script shows, and one `VERDICT first-day:` line the secretary can relay as it is. The scratch place is removed when no check failed, and kept (with `-events.txt` and `-messages.txt` beside the results) when one failed; `cleanup` removes it. The real organization is read once (`makiba status`, before the scratch place exists) and never written; nothing is pushed or landed, no pull request is made, and the scratch worker's run repository is abandoned, not pushed.

The checks, each a `CHECK` line:

1. **The binary** (`binary`, `real-binary`). The script builds this checkout (`cargo build`) and records the commit: `PASS`. For the real organization's `makibad` it reads the pid from `makiba status` (read only) and the program it runs and its start time from `/proc`, and prints them as `INFO`. A binary carries no commit (its version is `0.0.0`), so what the real `makibad` was built from cannot be read, and the script says so. It then always prints the two lines that restart the real organization on this build: `makiba down`, then `makiba up` (the first suspends the runs at work, the second brings them back).
2. **A code worker can build** (`code-worker-build`). In the scratch organization, `makiba` is registered with the hosts of section 5 and one code task is dispatched: add one line to `README.md` and one trivial test, then `cargo fetch`, `cargo build`, `cargo test --test first_day_check` and a commit. The worker gives cargo a `CARGO_HOME` of its own inside its run repository, so the first fetch must download every crate from the listed hosts, and it reports the number of crates it downloaded (`DOWNLOADED=<n>`). `PASS` only when the done report says `BUILD=ok` and `TEST=ok` with `n` above 0 and no refused host shows. With `n` 0 the result is `INFO` ("built from a warm cache: the hosts are not proven") and a `NOT SHOWN` line. On a refused host it is a `FAIL` that names the host and prints the line that adds it (`makiba config set network_hosts --project makiba <the list>,<host>`, and the same list for the next run in `MAKIBA_RUNBOOK_HOSTS`). **No event names a refused network host**: the refusal is Claude Code's. So the script reads the worker's own messages and the run's events for the words a refused host leaves in cargo's text (could not resolve, failed to connect, proxy, tunnel) and for the host named beside them; the brief tells the worker to report `blocked` with the first lines of such an error, as printed.
3. **A real diff review passes** (`review-diff`, `review-fresh-session`). The same worker calls the tool `review` with kind `diff` once. The script reads the answer the door stored and the `codex_round_started` event: `PASS` when the verdict is `passed` (with the reader's name); `FAIL` when it is `not-established`, with the reason copied whole into the results file (Issue 280 made it list the commands the reader ran, and that list is what tunes the recognizer). When the reader was the other model's program, the script abandons that run, stops the scratch `makibad` and starts it again with `MAKIBA_REVIEW_OTHER` pointing at a path that is not there (`Programs::from_env` in `src/daemon/review.rs` reads it when `makibad` starts, and `resolve` takes a path as it is, so the other reader is "not on PATH"; it is set in the script's own process, so for the scratch organization only), dispatches a second tiny worker (a README line and the trivial test, no cargo), and expects `passed` with the reader `fresh-session`. It is a second worker and not a second round because `makibad` reads the reader only at its start and a worker does not survive the restart. `INFO` when the first reader was already the fresh session.
4. **The landing gate's question reaches the person** (`gate-ask`, `gate-door`; the rest is a named step). The gate answers `ask` only for a real pull request: it reads `gh pr view`, needs a passed `ci_completed` at that head and fetches the head into the clone (`gate_outside` in `src/daemon/door.rs`), and anything less is a refusal the hook turns into a deny. So the script feeds the real hook, `makiba hook merge-gate`, the payload of the secretary's one allowed landing command, against a stand-in door in the scratch place that answers as the gate does under `merge_approval_mode` `human` for a head with no diff review (allow, warn, a reason holding `WARNING`), and reads the hook's JSON: `permissionDecision` `ask` with the `WARNING` line (`gate-ask`). It also asks the scratch organization's real door once for a pull request that does not exist, and expects the gate's refusal (`gate-door`: it shows the gate key and the door answer). **This proves the hook's answer. It does not prove the gate's decision on a real head, and it does not prove that Claude Code shows the question.** What remains is a named step for the person, printed as a `NOT SHOWN` line: in the real secretary's session, when the first pull request of a code task is to be landed with a head that has no passed diff review, the secretary runs `gh pr merge <n> --repo suisya-systems/makiba --squash --match-head-commit <full head sha>`; the person looks for a question that carries `WARNING review of <head>: none`. A command that runs with no question is a failure of the hook's wiring, to be written in the results.

The verdict is `READY: every check passed`, or `READY on what a script can show; left for the person: the NOT SHOWN lines` (always the case until the named step is done), or `NOT READY` with the `NEXT` lines. The stage needs the preconditions of section 4 (the same as `rehearse`), and the worker works from the project's `origin/main` on GitHub, not from this checkout: only the `makibad` is this checkout's build.

## 11. The switch (stage `switch`, Issue 51)

The owner moves makiba's development onto makiba itself. The person types one line in a login shell on the host, in their checkout of this repository, and then two lines to the secretary and one to approve a range. Nothing long is written or copied by hand: the stage prints each line whole, with the paths filled in.

```sh
bash scripts/runbooks/self-hosting.sh switch --handover <the previous secretary's handover file> --issues 169,270
```

`--issues` is optional: without it the line to the secretary tells it to take the Issues from section 1 of the handover file (the script holds no Issue number). `--dry-run` prints every command and changes nothing. The stage asks once (`yes`, or `--yes`), after it has printed what it is about to change.

### 11.1 What it does, in this order

Everything that can refuse comes first, so a refusal stops nothing:

1. **Read only.** Not inside an agent; the tools; no test seam; `makiba status --json`: a run in work (`queued`, `in_use`, `review`) or a pending entry unanswered refuses, with the list (a `suspended` or `failed` run is only noted: `up` brings a suspended run back). The checkout must be on `main`, with no uncommitted change to a tracked file, and `git fetch origin` must show `HEAD` an ancestor of `origin/main` (a fast-forward; the script never resets or merges). `MAKIBA_MERGE_APPROVAL_MODE` must be unset or `human` (11.4). The state folder must lie outside the checkout (a `MAKIBA_STATE_DIR` inside it refuses, since the handover and the start file would land in a public repository). The handover, when named, must be a readable, non-empty regular file, and lie outside the checkout; naming the file already placed is allowed and keeps it.
2. **Confirm.** Then `makiba down --secretary`: `makibad` and the worker terminals stop and the old secretary's terminal ends, so that the new secretary starts on the new role files and hooks. The base is not stopped (an update of the base is one of the three events of Issue 51 and is made on its own).
3. **`git merge --ff-only origin/main`, then `cargo build`.** The build waits until `makibad` is stopped: a binary replaced under a running `makibad` leaves its hook and channel paths stale (`docs/installing.md`: down, reinstall, up). `MK=<path>` names a binary that is used as it is, without a build.
4. **`makiba up`** (with the secretary). The stage waits (up to 10 minutes, `WAIT_SECRETARY`) for a secretary launch newer than the last one to be `verified` (an observed first turn) and for its terminal to be listed by `makiba status`. It prints `CHECK merge-approval-mode` from `makibad`'s own environment (`/proc/<pid>/environ`).
5. **The read surface.** `CHECK verb-range`, `verb-review` and `verb-goal`: each `PASS` when the new binary's `--help` lists it, else `FAIL` and the line that rebuilds. Then the full `makiba status`. A binary carries no commit (version 0.0.0), so the commits are in the start file (11.5), not in the binary.
6. **The start file and the handover** (11.3 and 11.5), then the lines to type (11.2).

Kept as they are, and said before the question: the goal ledger already placed (the stage never calls `place_ledger`), the project registration and its network hosts, the clone, the keys, the base, every event and message.

**Every failure after the stop names the next line**: a `RECOVER:` line, then `RESULT self-hosting switch: failed`. A failed `down` (it waits until `makibad` has let go of its lock, ends the terminals itself, and its exit status is the answer; `makiba status` is not used, since it shows `makibad` as running for 30 seconds after a stop): `makiba status`, `makiba up` if the organization is not as it was, then run the stage again; the checkout and the binary were not touched. A failed fast-forward or build: the old binary is still at its place and nothing was suspended, so `makiba up` brings the organization back as it was; fix the cause and run the stage again. A failed `up`, or a secretary whose launch failed or was not ready: `makiba up` again (it asks for the secretary again; the screen is in the `worker_spawn_failed` event, `makiba events --after <id> --json`). The stage is safe to run again at each of these points. The script ends in `exit` inside its branch because the fast-forward can change the script's own file while the shell runs it.

### 11.2 What the person types, in order

The stage ends with these, filled in (the attach line is taken from the `secretary:` line of `makiba status`):

1. `HC_SESSION=<session> hc attach secretary` in the login shell.
2. To the secretary: `Read <state>/secretary/handover-owner.md first: it is the handover from the previous organization's secretary (my decisions and how I want to be spoken to). It is private: never copy it into a repository, an issue, a pull request, a document, a message to a worker or a note. Then call status and tell me in three lines where we stand.`
3. To the secretary, the first range: `Propose the first range for makiba: the Issues 169,270, 2 at once, 20 tasks in all, the brief head of our usual conventions. Show me what it admits and what it leaves out, with the reason.` (The owner's decision of 2026-11-22: two at once, every open Issue that needs no host and is not labelled `backlog`, and a budget of twenty tasks, the measure of Issue 51. With no `--issues` the line says "the Issues listed in section 1 of <the handover>".) The secretary records it with `range propose`, reads `makiba status` (`ranges[].candidates`: each Issue admitted, or left out with the reason) and shows the person that list. Nothing is dispatched.
4. In the person's own shell, with the id the secretary names: `makiba range approve <id>`. Only the person's key approves. It prints, before it asks `Approve range <id>? [y/N]`: the project and status, `Issues:`, `Takes:`, `At once: 2; tasks in all: 20`, `Merge approval mode (this shell's; makibad records its own): <mode>`, the `Brief head:`, and `Read at <time> - in the order they would go in:` with one line per Issue, `#<n> <title> - admitted` or `left out: <why>`. The person checks: the mode is `human`; the Issues and the condition are what the owner decided; the order is acceptable; an Issue at a trust boundary (git, the sandbox, keys, the push line) says "stop after the design" in the brief head. Then `y`. `has no complete read yet` means wait for `makibad` to read the draft; `stale` means it read the draft again after the preview, so run the line again. A range is never widened afterwards: more Issues or budget is a new proposal, approved again.

### 11.3 The handover: where it is placed, and why there

The previous organization's secretary wrote a handover for makiba's secretary: the owner's decisions and how the owner wants to be spoken to. It is operator-private and the repository is public, so it must never be committed, quoted in an Issue or a pull request, or put in a document.

Place: **`<state>/secretary/handover-owner.md`**, mode 0600 (`<state>` is `$MAKIBA_STATE_DIR`, default `${XDG_STATE_HOME:-~/.local/state}/makiba`). Why, from the code:

- That folder is the secretary's own working folder and its write surface (`prepare.rs`: `write_dirs` starts with the launch's folder; spec section 2). It is outside every repository. `makibad` only creates it if it is missing (`create_dir_all`); it is not emptied at a launch, so the file survives a renewal and a restart.
- The secretary's settings deny it `state.db` (and its `-wal`/`-shm`), the keys, the base's and the core's folders, and nothing else of the state folder, so it can read the file. Its rules say a folder outside the registered repositories is the person's own and is never read unless the person names that path: the first line of 11.2 names it.
- The knowledge folder is not used: the curator reads and merges it, and its notes can become skill candidates, so private text would reach the curator. The documents folder holds the folders of document workers. The `note` verb needs a task, goes to the curator as a raw note with four headings, and is the wrong audience. `makiba renew --handover-file <file>` would also reach the secretary (as the first brief of a fresh secretary), but the text would be stored in a row of `state.db` for good and is limited to 32 KiB; it is not used.

`--handover <file>` copies the file there with mode 0600, refuses a file inside the checkout, keeps an older different copy as `handover-owner.md.before-<UTC>`, and leaves the original where it is. Without the flag the stage prints the one line that does it: `mkdir -p -m 700 <state>/secretary && install -m 600 <file> <state>/secretary/handover-owner.md`.

### 11.4 The merge approval mode: human first

The first range runs under the `human` mode, the owner's decision of 2026-11-22. The purpose is to gather what is needed to decide on `acceptance`: (1) to see, on the first real code pull request, that the agent host shows the landing gate's question to the person (only the human mode can show it; `first-day` proved the hook's answer, not what Claude Code shows: section 10, check 4); (2) to judge the secretary's recommendations from the stops on record. After ten recorded stops the owner decides, from how many answers matched the recommendation, whether to switch to `acceptance`.

Where it is set: **only** the environment variable `MAKIBA_MERGE_APPROVAL_MODE`, read by `makibad` from its own environment before every answer of the gate (`src/daemon/gate.rs`), which `makiba up` passes on from the shell that runs it. `config set merge_approval_mode` is "not built yet" and `config get` always answers null, so there is no second place and nothing to win over it. Unset, `human`, or any value but `acceptance` is `human`. `makiba status` and `makiba range approve` print the value of **their own** shell; the stage prints `makibad`'s from `/proc` after `up`. The stage refuses to go on when this shell holds a value that is not `human`, and says the one line that clears it: `unset MAKIBA_MERGE_APPROVAL_MODE`.

The change to `acceptance` is the owner's, and it is this one line, typed when nothing is in work:

```sh
makiba down --secretary && MAKIBA_MERGE_APPROVAL_MODE=acceptance makiba up
```

A range approved under `human` does not widen when the mode changes: it needs its own approval again (`range` records the mode it was approved under).

### 11.5 The record for Issue 51

The stage writes **`<state>/self-hosting-start.txt`** once and never overwrites it (a second run prints the first): the UTC time, `origin/main` and the checkout's full commit, `count_after_event_id` (the newest event id after `up`), the base version of the newest `dispatcher_started`, and the mode in force. The count begins there and is read afterwards from the read surface only, never the tables:

```sh
sed -n 's/^count_after_event_id: //p' <state>/self-hosting-start.txt     # N
makiba events --after N --json
makiba status --json
```

- **Twenty pull requests landed through makiba:** the events of kind `pr_merged` after `N` (`makiba status --json`: `ranges[].items[].place` equal to `landed` shows the same landings per range). **`pr_merged` is written by the PR watch (`followed` in `src/daemon/prwatch.rs`, spec sections 5.6 and 5.8) for a run's current pull request, while no `pr_merged` of it is recorded, when the run is open (`queued`, `in_use`, `review`, `suspended` or `parked`), is a `completed` run of a range, is a task an open design-only run waits for and not `abandoned`, or, unless its pull request was read closed, is a run outside a range `completed` or any run `abandoned` less than a day before.** A landing outside that is not in it: a run outside a range whose pull request merged more than a day after its close, and, before Issue 385, a run closed with `--abandon` whose pull request merged after the last read while it was open (the range tasks r3-299, r3-317 and r3-335 most likely: each merged less than a minute before its close, which the read before Issue 388 did not cover, and the closes were asked for with `--abandon`). The twenty are counted from the `pr_merged` events, each pull request once; a landing with no `pr_merged` event is added by hand, with its pull request number, in `docs/reports/self-hosting-week.md`.
- **The three events:** an update of the base is a `dispatcher_started` after `N` whose `base_version` differs from the start file's; a renewal of the secretary is `secretary_renewed`; a failed launch is `worker_spawn_failed`. One that has not happened by itself when the count is reached is brought about on purpose (Issue 51) and recorded as such.
- Incidents met go to `docs/design/incidents.md` and an Issue for their test, as before.
