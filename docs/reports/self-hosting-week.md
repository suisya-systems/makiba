# Report: makiba's first week on makiba (Issue 51)

This is the record that Issue 51 asks for. It is a skeleton: what the records already give is filled in, and every count that needs the ledger is a visible `TODO(events)` with the exact query that gives it. No number here was estimated. The owner closes Issue 51 on this record once every `TODO(events)` is filled.

How the count is read is in `docs/runbooks/self-hosting.md` section 11.5. Times are UTC unless a time is marked JST (UTC+9).

## 1. The count's start

From `<state>/self-hosting-start.txt`, written once by the `switch` stage:

| Field | Value |
| --- | --- |
| Start (UTC) | 2026-11-22T10:19Z |
| `count_after_event_id` (N) | 49 |
| `origin/main` and the checkout's commit at the start | TODO(events): `grep -E '^(origin_main\|checkout_head): ' <state>/self-hosting-start.txt` |
| Base version at the start | TODO(events): `sed -n 's/^base_version: //p' <state>/self-hosting-start.txt` |
| Merge approval mode in force | TODO(events): `sed -n 's/^merge_approval_mode: //p' <state>/self-hosting-start.txt` |

The start time and N above are as the owner gave them; `grep -E '^(started_at_utc|count_after_event_id): ' <state>/self-hosting-start.txt` confirms them.

### 1.1 Reading the ledger

`makiba events` returns at most 1000 events per call, so the events after N are first gathered into one file, and every query below reads that file. The person runs this once, on the host, when the count is reached:

```sh
E=$(mktemp "${TMPDIR:-/tmp}/events-51.XXXXXX") || echo "mktemp failed; nothing is written" >&2
after=49
while :; do
  page=$(makiba events --after "$after" --limit 1000 --json) || { echo "makiba events failed; $E is incomplete" >&2; break; }
  n=$(printf '%s' "$page" | jq '.events | length') || { echo "unreadable page; $E is incomplete" >&2; break; }
  [ "$n" -eq 0 ] && { echo "done: $(wc -l < "$E") events in $E"; break; }
  printf '%s' "$page" | jq -c '.events[]' >> "$E" || { echo "write failed; $E is incomplete" >&2; break; }
  after=$(printf '%s' "$page" | jq '.events[-1].id')
done
```

Use the file only when the loop printed `done:`; any other line means it is incomplete and the loop is run again.

Each event has `id`, `occurred_at`, `actor`, `kind`, `task_id` and `payload` (`docs/design/spec.md`, table `Event field`).

## 2. Counts

| What | Count | Query |
| --- | --- | --- |
| Runs | TODO(events) | `jq -r 'select(.kind=="run_taken") \| .task_id' "$E" \| sort -u \| wc -l` (distinct tasks taken; a relaunch is also a `run_taken` of the same task, so tasks are counted, not events) |
| Failed launches | TODO(events) | `jq -c 'select(.kind=="worker_spawn_failed") \| {id, occurred_at, task_id, stage: .payload.stage, seen: .payload.seen}' "$E"` (count the lines; a launch without a run, such as the curator's, has `task_id` null) |
| Undelivered messages at any point | TODO(events) | Not readable from `makiba events` or `makiba status`: delivery rows are not on the read surface (`docs/design/spec.md` section 12, "The read surface"). The closest records are `jq -c 'select(.kind=="observation_lost" or (.kind=="anomaly_raised" and .payload.rule=="secretary_still"))' "$E"`. The person decides the source of this count (makibad's log, or a read of the delivery rows added to the read surface) before it is filled; until then it stays open. |
| Renewals of the secretary | TODO(events) | `jq -c 'select(.kind=="secretary_renewed") \| {id, occurred_at, actor, payload}' "$E"` (count the lines; at least event 202, section 4.2) |
| Time `makibad` was down | about 3 h 07 min known (section 3); total TODO(events) | `jq -c 'select(.kind=="dispatcher_started" or .kind=="dispatcher_stopped") \| {id, occurred_at, kind, base_version: .payload.base_version}' "$E"`: each stop and the next start is a down time. A start with no clean stop before it (a crash) has no recorded start of its down time: the last event before it gives only an upper bound, so its start is taken from other evidence (the person's word, a log) and the source is named, as in section 3 |
| Pull requests landed through makiba | TODO(events): provisional 18 landed and 6 pending (section 5) | The list: `jq -c 'select(.kind=="pr_merged") \| {id, repo: .payload.repo, pr: .payload.pr, merged_at: .payload.merged_at, task_id}' "$E"`. The count, each pull request once: `jq -r 'select(.kind=="pr_merged") \| "\(.payload.repo)#\(.payload.pr)"' "$E" \| sort -u \| wc -l`, plus the hand-added landings of section 5 that have no `pr_merged` event |

## 3. Time `makibad` was down

| From | To | Length | Cause |
| --- | --- | --- | --- |
| 2026-11-22 ~11:56 (2026-11-22 20:56 JST) | 2026-11-22 15:03 (2026-11-23 00:03 JST) | about 3 h 07 min | The whole WSL instance went down. `makibad` was restarted with the organization at 15:03 UTC. |

The start time is the person's word: the ledger's last act before it is the dispatch of r2-288 at 11:48, so the ledger alone bounds this down time at 3 h 15 min at most. The end is `makibad`'s new heartbeat and the relaunched terminals at 15:03. The workers resumed; no task was dispatched again.

Other down times: TODO(events), from the `dispatcher_started` and `dispatcher_stopped` query of section 2 (the clean `makiba down` and `makiba up` of 2026-11-23 in section 7.2 is one of them).

## 4. The three events

An update of the base is a `dispatcher_started` after N whose `base_version` differs from the start file's; a renewal of the secretary is `secretary_renewed`; a failed launch is `worker_spawn_failed` (`docs/runbooks/self-hosting.md` section 11.5).

| Event | Met | When | On purpose? |
| --- | --- | --- | --- |
| Failed launch | yes | 2026-11-22T19:06:42Z | No, met by itself |
| Renewal of the secretary | yes | 2026-11-22T10:46Z, event 202 | TODO(events): whether it was asked for (`actor` of event 202 and the `renew` that led to it) |
| Update of the base | not yet | | To be brought about on purpose |

### 4.1 Failed launch, met by itself

At 2026-11-22T18:59:55Z the curator was launched. At 19:06:42Z `worker_spawn_failed` was written at stage `launched`: no working status or turn within 180000 ms, the screen showing Claude Code's dev-channel startup dialog. At 19:29:57Z the curator's 30-minute limit ended the terminal. The curator never ran.

The mechanism, read with medium confidence: `makibad`'s loop was held for minutes, and the verify pass ran before the dialog step. Fixed by Issue 333 (pull request 334). No repair from claude-org-ja was made. Incident row: I-18.

Event id: TODO(events): `jq -c 'select(.kind=="worker_spawn_failed" and (.occurred_at | test("^2026-11-22.19:06")))' "$E"`

### 4.2 Renewal of the secretary

Event 202, `secretary_renewed`, at 2026-11-22T10:46Z, as stated by the owner from the ledger. To check: `jq -c 'select(.id==202)' "$E"`.

### 4.3 Update of the base

Not met. It is to be brought about on purpose, by a pin move (`docs/upgrading-the-base.md`), on a day the owner sets.

- Day set by the owner: TODO
- The `dispatcher_started` that shows it: TODO(events): `jq -c --arg v "<base_version of the start file>" 'select(.kind=="dispatcher_started" and .payload.base_version != $v) | {id, occurred_at, base_version: .payload.base_version}' "$E"`

## 5. Pull requests landed through makiba

Twenty are needed. A pull request landed by a repair from claude-org-ja is not one of the twenty. `pr_merged` is written for the tasks of a range, and also for a one-off run that was parked or that a design-only run waits for (`landing_candidates` in `src/daemon/belt.rs`); any other one-off landing is not in it. The count is the pull requests of the `pr_merged` events, each pull request once, plus by hand only the landings that have no `pr_merged` event, each named with its pull request number here. A pull request is never counted twice.

**Provisional:** the list below is not yet checked against the `pr_merged` events (query in section 2). Each row's last column is filled from that query.

| Pull request | State | Note | `pr_merged` event |
| --- | --- | --- | --- |
| 301 | landed | | TODO(events) |
| 304 | landed | Merged by the owner in the web UI, not through makiba's landing; whether it counts is the owner's call | TODO(events) |
| 305 | landed | | TODO(events) |
| 309 | landed | Its run's close needed a repair from claude-org-ja (section 7.1); the pull request itself landed through makiba and counts | TODO(events) |
| 310 | landed | | TODO(events) |
| 311 | landed | | TODO(events) |
| 314 | landed | | TODO(events) |
| 319 | landed | | TODO(events) |
| 321 | landed | | TODO(events) |
| 323 | landed | | TODO(events) |
| 324 | landed | | TODO(events) |
| 325 | landed | | TODO(events) |
| 328 | landed | | TODO(events) |
| 329 | landed | | TODO(events) |
| 334 | landed | | TODO(events) |
| 336 | landed | | TODO(events) |
| 342 | landed | Its run was closed with `abandon` (section 7.3); the work landed | TODO(events) |
| 346 | landed | | TODO(events) |
| 347 | pending | Approved, landing | TODO(events) |
| 348 | pending | Approved, landing | TODO(events) |
| 349 | pending | Approved, landing | TODO(events) |
| 350 | pending | Approved, landing | TODO(events) |
| 351 | pending | Approved, landing | TODO(events) |
| 353 | pending | Approved, landing | TODO(events) |

Provisional tally: 18 landed (17 through makiba's landing and 304 in the web UI), 6 pending. Final count: TODO(events).

## 6. Who "the person" was

As stated by the owner, for 2026-11-22 and 2026-11-23: the person's side of the conversation was carried by claude-org-ja, acting for the owner, and it directed in detail, down to the order of the work and the content of Issues. The person's involvement recorded in that period (answers, merges, orders, wording) is therefore larger than a person's involvement really is, and it should not be read as what makiba needs from a person.

From 2026-11-23 the person answers only at a person's grain. The gaps that were closed by word were turned into Issues: 337, 338, 339, 340, 341, 343 and 344.

## 7. Repairs and help from claude-org-ja

Each has its row in `docs/design/incidents.md`.

### 7.1 The close of r2-288 (row I-15, check Issue 318)

The close of r2-288 (pull request 309, merged) was refused because the worktree held 12 untracked sandbox mask placeholders: empty read-only dotfiles created at 20:54 JST on 2026-11-22, two minutes before the WSL crash. The worker could not remove them. The owner checked that all 12 were 0 bytes and removed them from claude-org-ja's side; the run closed as completed. Pull request 309 landed through makiba and counts.

### 7.2 The makiba binary replaced under a running `makibad` (row I-19, checks Issues 340 and 352)

On 2026-11-23 claude-org-ja missed that `makiba down` had been refused, and rebuilt and replaced the makiba binary under the running `makibad`. For about 10 minutes the old `makibad` ran with the new binary (6aa388e) on disk; the owner then ran `makiba down` and `makiba up`. This is an incident from claude-org-ja's side, not a repair of makiba's work.

### 7.3 park-race-332 closed with `abandon` (row I-20, check Issue 320)

On 2026-11-23 park-race-332 (Issue 332, pull request 342, merged) was closed with `abandon` by the owner's decision although its work landed. The cause was the secretary's: to push the test commit alone while the worker's prototype was uncommitted, the secretary had the worker commit it on a local side branch. Its change went into the fix by cherry-pick, the side branch stayed unmerged, the hooks refuse its forced deletion, and `close` refused the unpushed commit. Nothing was lost.

### 7.4 The CI log of pull request 324 (row I-17, checks Issues 327 and 341)

The secretary could not read a CI log of pull request 324, so it was read from claude-org-ja. claude-org-ja's reading of the cause was passed to the worker as a finding, then corrected to a hypothesis. Row I-17 names Issue 327 as its check; Issue 341 is the second check.

### 7.5 Not a repair

The failed launch of section 4.1 (row I-18, Issue 333) needed no repair from claude-org-ja.
