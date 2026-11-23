# Report: makiba's first week on makiba (Issue 51)

This is the record that Issue 51 asks for. It began as a skeleton (pull request 359) and its counts are now filled from an export of the ledger (section 1.2). No number here was estimated. What the export cannot give stays a visible `TODO`, with its reason: `TODO(events)` where a record outside the export would give it, `TODO(person)` where only the person can say. The owner closes Issue 51 on this record once every `TODO` is filled or accepted as open.

How the count is read is in `docs/runbooks/self-hosting.md` section 11.5. Times are UTC unless a time is marked JST (UTC+9).

## 1. The count's start

From `<state>/self-hosting-start.txt`, written once by the `switch` stage:

| Field | Value |
| --- | --- |
| Start (UTC) | 2026-11-22T08:47:27Z (the start file; the skeleton had 10:19Z, see below) |
| `count_after_event_id` (N) | 49 |
| `origin/main` and the checkout's commit at the start | both ef12449 (pull request 294, the switch stage of the runbook and the start of the count) |
| Base version at the start | 0.1.21. The ledger agrees: every `dispatcher_started` and `dispatcher_stopped` from event 98 up to event 6864 shows `base_version` 0.1.21 |
| Merge approval mode in force | `human`. The status taken with the export (section 1.2) also shows `human` |

The rows above were read from a copy of the start file the owner placed beside the export, with `grep -E '^(started_at_utc|count_after_event_id|origin_main|checkout_head|base_version|merge_approval_mode): ' <state>/self-hosting-start.txt`. N, the base version and the merge approval mode agree with the ledger and the status. The start time does not agree with what the skeleton had: the file says 08:47:27Z, and the first event after N, event 50, occurred at 08:47:28Z, which fits the file. The 10:19Z the skeleton had (as the owner gave it) is not in the file; this report uses the file's time.

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

### 1.2 The export the counts were read from

The owner exported the ledger on 2026-11-23 at about 15:47Z (2026-11-24 00:47 JST): the pages of `makiba events --after N --json` joined into one object `{"events": [...]}`, and `makiba status --json` taken at the same time.

- Events: after id 49 up to id 6925, 6876 events, no id missing, in id order (checked when the counts were made). The first, event 50, occurred at 2026-11-22T08:47Z; the last, event 6925, at 2026-11-23T15:47:19Z.
- Status: `makibad` running, heartbeat at 2026-11-23T15:47:15Z.
- The queries of section 2 were run over that file, read as one array instead of one event per line; the filters are the same.
- The count follows N, as section 11.5 of the runbook does. Event 50 occurred one second after the start file's time (section 1), so the event range and the start time agree.
- Events after 6925, and what happened after the export, are not counted here.

## 2. Counts

| What | Count | Query |
| --- | --- | --- |
| Runs | 46 tasks (56 `run_taken` events) | `jq -r 'select(.kind=="run_taken") \| .task_id' "$E" \| sort -u \| wc -l` (distinct tasks taken; a relaunch is also a `run_taken` of the same task, so tasks are counted, not events) |
| Failed launches | 1 (event 3068, the curator's, `task_id` null, stage `launched`; section 4.1) | `jq -c 'select(.kind=="worker_spawn_failed") \| {id, occurred_at, task_id, stage: .payload.stage, seen: .payload.seen}' "$E"` (count the lines; a launch without a run, such as the curator's, has `task_id` null) |
| Undelivered messages at any point | 0 `observation_lost` events, the source the owner chose | `jq -c 'select(.kind=="observation_lost")' "$E"` (count the lines). Delivery rows are not on the read surface (`docs/design/spec.md` section 12, "The read surface"), so the owner decided that this count is read from `observation_lost`. What that event records is narrower: a record the monitor dropped because it could not be written within the retry window (`docs/design/spec.md` section 10). Zero of them does not show from the delivery rows that no message stayed undelivered; that would need the delivery rows on the read surface |
| `secretary_still` anomalies (read by the owner as messages delivered but not acted on) | 10 anomalies | `jq -c 'select(.kind=="anomaly_raised" and .payload.rule=="secretary_still")' "$E"` (count the lines). The rule is raised when a worker anomaly is open while the secretary's status stays unchanged for 600 s, or a `pending` entry stays open for 600 s (`docs/design/spec.md` section 11). Each is one anomaly, not one message, and it says nothing of delivery; the owner counts them apart from the undelivered messages |
| Renewals of the secretary | 1 (event 202; section 4.2) | `jq -c 'select(.kind=="secretary_renewed") \| {id, occurred_at, actor, payload}' "$E"` (count the lines; at least event 202, section 4.2) |
| Time `makibad` was down | about 3 h 15 min in five down times: one crash of about 3 h 07 min (the person's word; the ledger bounds it at 3 h 10 min) and four clean stops of 7 min 35 s in all (section 3) | `jq -c 'select(.kind=="dispatcher_started" or .kind=="dispatcher_stopped") \| {id, occurred_at, kind, base_version: .payload.base_version}' "$E"`: each stop and the next start is a down time. A start with no clean stop before it (a crash) has no recorded start of its down time: the last event before it gives only an upper bound, so its start is taken from other evidence (the person's word, a log) and the source is named, as in section 3 |
| Pull requests landed through makiba | 34: 12 with a `pr_merged` event and 22 added by hand (section 5); 33 if pull request 304 is left out | The list: `jq -c 'select(.kind=="pr_merged" and (.payload.repo \| ascii_downcase) == "suisya-systems/makiba") \| {id, repo: .payload.repo, pr: .payload.pr, merged_at: .payload.merged_at, task_id}' "$E"`. The count, each pull request once: `jq -r 'select(.kind=="pr_merged" and (.payload.repo \| ascii_downcase) == "suisya-systems/makiba") \| "\(.payload.repo)#\(.payload.pr)"' "$E" \| sort -u \| wc -l`, plus the hand-added landings of section 5 that have no `pr_merged` event. Only makiba's own repository counts: another registered project's landings are left out |

## 3. Time `makibad` was down

From the `dispatcher_started` and `dispatcher_stopped` query of section 2: four stops each followed by a start, and one start (event 709) with no stop before it, the crash.

| From | To | Length | Cause | Events |
| --- | --- | --- | --- | --- |
| 2026-11-22 10:13:19 | 2026-11-22 10:13:24 | 5 s | A clean stop and start | 98, 99 |
| 2026-11-22 ~11:56 (2026-11-22 20:56 JST) | 2026-11-22 15:03:40 (2026-11-23 00:03 JST) | about 3 h 07 min | The whole WSL instance went down. `makibad` was restarted with the organization at 15:03. | 709 (no stop) |
| 2026-11-23 03:01:32.557 | 2026-11-23 03:01:32.987 | under 1 s | The owner's `makiba down` and `makiba up` after the binary was replaced (section 7.2) | 3468, 3470 |
| 2026-11-23 10:20:43 | 2026-11-23 10:20:54 | 10 s | A clean stop and start | 5330, 5331 |
| 2026-11-23 15:36:45 | 2026-11-23 15:44:04 | 7 min 19 s | The restart on the base 0.1.22 (section 4.3) | 6864, 6869 |

The start time of the crash is the person's word. The ledger's last event before it is event 703 (a `terminal_status`) at 11:53:43, so the ledger alone bounds this down time at 3 h 10 min at most. The end is `makibad`'s new heartbeat and the relaunched terminals at 15:03. The workers resumed; no task was dispatched again.

In all, about 3 h 15 min (at most 3 h 18 min by the ledger's bound). The causes of the stops at 10:13 and 10:20 are not in the ledger. Between the stop of event 6864 and the start of event 6869, two runs were taken at 15:42:14 (events 6865 and 6866) and again at 15:44:04 with the start; the down time is counted from the stop to the `dispatcher_started`, as the query says.

## 4. The three events

An update of the base is a `dispatcher_started` after N whose `base_version` differs from the start file's; a renewal of the secretary is `secretary_renewed`; a failed launch is `worker_spawn_failed` (`docs/runbooks/self-hosting.md` section 11.5).

| Event | Met | When | On purpose? |
| --- | --- | --- | --- |
| Failed launch | yes | 2026-11-22T19:06:42Z, event 3068 | No, met by itself |
| Renewal of the secretary | yes | 2026-11-22T10:46:40Z, event 202 | Not by the `renew` verb: it was `makibad`'s relaunch of a secretary whose terminal had exited (section 4.2). TODO(person): whether that exit was brought about on purpose is not in the ledger |
| Update of the base | yes | 2026-11-23T15:44:04Z, event 6869 | Yes, by the pin move to herdr-core 0.1.22 (pull request 373) and a restart by the owner |

### 4.1 Failed launch, met by itself

At 2026-11-22T18:59:55Z the curator was launched. At 19:06:42Z `worker_spawn_failed` was written at stage `launched`: no working status or turn within 180000 ms, the screen showing Claude Code's dev-channel startup dialog. At 19:29:57Z the curator's 30-minute limit ended the terminal. The curator never ran.

The mechanism, read with medium confidence: `makibad`'s loop was held for minutes, and the verify pass ran before the dialog step. Fixed by Issue 333 (pull request 334). No repair from claude-org-ja was made. Incident row: I-18.

Event id: 3068, the only `worker_spawn_failed` after N, `task_id` null, stage `launched`, at 2026-11-22T19:06:42Z. Query: `jq -c 'select(.kind=="worker_spawn_failed" and (.occurred_at | test("^2026-11-22.19:06")))' "$E"`

### 4.2 Renewal of the secretary

Event 202, `secretary_renewed` (actor `makibad`), at 2026-11-22T10:46:40Z, the only one after N. Checked with `jq -c 'select(.id==202)' "$E"`.

It was not asked for with `renew`. Just before it, event 195 records the secretary's terminal gone (reason `exited`, exit code 0) at 10:46:36Z, and event 196 raises `secretary_gone`. That rule is raised only while no `makiba renew` is under way (`docs/design/spec.md` section 11), and on it `makibad` relaunches the secretary by the path of `renew` with a row of its own; events 197 to 202 are that launch, its verification and the renewal. What made the secretary's terminal exit is not in the ledger: TODO(person), whether it was brought about on purpose.

### 4.3 Update of the base

Met on 2026-11-23, brought about on purpose by a pin move (`docs/upgrading-the-base.md`): the pin moved to herdr-core 0.1.22 by pull request 373 (task base-0-1-22, merged at 15:41Z), and the owner restarted `makibad` on it.

- Event 6864, `dispatcher_stopped` at 15:36:45Z, `base_version` 0.1.21: the last stop on the old base.
- Event 6869, `dispatcher_started` at 15:44:04Z, `base_version` 0.1.22: the update. It is the only `dispatcher_started` after N whose `base_version` is not 0.1.21. Event 6870, `base_resynced` (reason `start`), follows it.
- Query: `jq -c --arg v "0.1.21" 'select(.kind=="dispatcher_started" and .payload.base_version != $v) | {id, occurred_at, base_version: .payload.base_version}' "$E"` (with 0.1.21 for the start file's `base_version`, which the ledger shows; section 1).

## 5. Pull requests landed through makiba

Twenty are needed. A pull request landed by a repair from claude-org-ja is not one of the twenty. `pr_merged` is written for the tasks of a range, and also for a one-off run that was parked or that a design-only run waits for (`landing_candidates` in `src/daemon/belt.rs`); any other one-off landing is not in it. The count is the pull requests of the `pr_merged` events, each pull request once, plus by hand only the landings that have no `pr_merged` event, each named with its pull request number here. A pull request is never counted twice.

**Checked** against the `pr_merged` events of the export (query in section 2). There are 12 such events, one per pull request, all of makiba's repository. A pull request is added by hand when it has no `pr_merged` event, its `pr_opened` event after N names a makiba task, and its merge commit is on `origin/main` before the export. The merge times are those of the commits on `origin/main`. No pull request was landed by a repair from claude-org-ja (section 7).

| Pull request | State | Note | `pr_merged` event |
| --- | --- | --- | --- |
| 301 | landed | | 546 |
| 304 | landed | Merged by the owner in the web UI, not through makiba's landing; whether it counts is the owner's call | 1417 |
| 305 | landed | | 2730 |
| 309 | landed | Its run's close needed a repair from claude-org-ja (section 7.1); the pull request itself landed through makiba and counts | 1619 |
| 310 | landed | | none: added by hand (`pr_opened` 1065) |
| 311 | landed | | none: added by hand (`pr_opened` 1109) |
| 314 | landed | | none: added by hand (`pr_opened` 1240) |
| 319 | landed | | none: added by hand (`pr_opened` 1684) |
| 321 | landed | | none: added by hand (`pr_opened` 2175) |
| 323 | landed | | none: added by hand (`pr_opened` 2209) |
| 324 | landed | | none: added by hand (`pr_opened` 2231) |
| 325 | landed | | none: added by hand (`pr_opened` 2460) |
| 328 | landed | | none: added by hand (`pr_opened` 2752) |
| 329 | landed | | none: added by hand (`pr_opened` 2874) |
| 334 | landed | | none: added by hand (`pr_opened` 3355) |
| 336 | landed | | none: added by hand (`pr_opened` 3546) |
| 342 | landed | Its run was closed with `abandon` (section 7.3); the work landed | none: added by hand (`pr_opened` 3628) |
| 346 | landed | | 4613 |
| 347 | landed | Range task r3-299 | none: added by hand (`pr_opened` 4059) |
| 348 | landed | Range task r3-317; the twentieth landing counting 304 (2026-11-23T07:17Z) | none: added by hand (`pr_opened` 4093) |
| 349 | landed | | none: added by hand (`pr_opened` 4152) |
| 350 | landed | Range task r3-335 | none: added by hand (`pr_opened` 4181) |
| 351 | landed | | 5075 |
| 353 | not landed | Range task r3-340; no merge on `origin/main`. Issue 340's change landed as pull request 362 | none |
| 359 | landed | This report's skeleton | none: added by hand (`pr_opened` 4787) |
| 362 | landed | | none: added by hand (`pr_opened` 5198) |
| 363 | landed | | 6050 |
| 364 | landed | | 5905 |
| 365 | landed | | 6121 |
| 366 | landed | | 5975 |
| 367 | landed | | 6416 |
| 370 | landed | | 6531 |
| 371 | landed | | none: added by hand (`pr_opened` 6088) |
| 373 | landed | The pin move to herdr-core 0.1.22 (section 4.3) | none: added by hand (`pr_opened` 6487) |
| 376 | landed | | none: added by hand (`pr_opened` 6704) |

Final count: 34 landed (12 with `pr_merged`, 22 added by hand); 33 if 304 is left out. The twentieth landing was 348 at 2026-11-23T07:17Z, or 347 at 07:29Z without 304. Not landed at the export: 353 (above), 303 and 308 (closed, events 5339 and 5338; their work landed as 310 and 323), and 313, 315, 316, 377 and 378 (no merge on `origin/main`).

Of the provisional list, every row marked landed is confirmed on `origin/main`; of the six pending, 347, 348, 349, 350 and 351 landed and 353 did not. Eleven landings were not on it: 359, 362 to 367, 370, 371, 373 and 376.

Pull request 296 merged on `origin/main` at 2026-11-22T10:00Z, after the start file's time, but no event after N names it (no `pr_opened`, no `pr_merged`), so the rule above does not add it. It is the fix of Issue 295, which Issue 51 records as a repair that could only be made from claude-org-ja, and so it is not one of the twenty in any case. It has no section in 7, and no row of `docs/design/incidents.md` names Issue 295: TODO(person), whether it needs one.

Three range tasks (r3-299, r3-317 and r3-335) landed with no `pr_merged` event, although `pr_merged` is written for the tasks of a range. Why is not established here.

## 6. Who "the person" was

As stated by the owner, for 2026-11-22 and 2026-11-23: the person's side of the conversation was carried by claude-org-ja, acting for the owner, and it directed in detail, down to the order of the work and the content of Issues. The person's involvement recorded in that period (answers, merges, orders, wording) is therefore larger than a person's involvement really is, and it should not be read as what makiba needs from a person.

From 2026-11-23 the person answers only at a person's grain. The gaps that were closed by word were turned into Issues: 337, 338, 339, 340, 341, 343 and 344.

## 7. Repairs and help from claude-org-ja

Each has its row in `docs/design/incidents.md`.

### 7.1 The close of r2-288 (row I-15, check Issue 318)

The close of r2-288 (pull request 309, merged) was refused because the worktree held 12 untracked sandbox mask placeholders: empty read-only dotfiles created at 20:54 JST on 2026-11-22, two minutes before the WSL crash. The worker could not remove them. The owner checked that all 12 were 0 bytes and removed them from claude-org-ja's side; the run closed as completed. Pull request 309 landed through makiba and counts.

### 7.2 The makiba binary replaced under a running `makibad` (row I-19, checks Issues 340 and 352)

On 2026-11-23 claude-org-ja missed that `makiba down` had been refused, and rebuilt and replaced the makiba binary under the running `makibad`. For about 10 minutes the old `makibad` ran with the new binary (6aa388e) on disk; the owner then ran `makiba down` and `makiba up` (events 3468 and 3470, 03:01:32Z). This is an incident from claude-org-ja's side, not a repair of makiba's work.

### 7.3 park-race-332 closed with `abandon` (row I-20, check Issue 320)

On 2026-11-23 park-race-332 (Issue 332, pull request 342, merged) was closed with `abandon` by the owner's decision although its work landed. The cause was the secretary's: to push the test commit alone while the worker's prototype was uncommitted, the secretary had the worker commit it on a local side branch. Its change went into the fix by cherry-pick, the side branch stayed unmerged, the hooks refuse its forced deletion, and `close` refused the unpushed commit. Nothing was lost.

### 7.4 The CI log of pull request 324 (row I-17, checks Issues 327 and 341)

The secretary could not read a CI log of pull request 324, so it was read from claude-org-ja. claude-org-ja's reading of the cause was passed to the worker as a finding, then corrected to a hypothesis. Row I-17 names Issue 327 as its check; Issue 341 is the second check.

### 7.5 Not a repair

The failed launch of section 4.1 (row I-18, Issue 333) needed no repair from claude-org-ja.
