# The screen: where the person puts work in, watches it and answers

Status: design note for Issue 169, 2026-11-22. Nothing is built. The drawing is [screen-draft.html](screen-draft.html): a static page with sample data, no script and no request; open it in a browser 1440 px wide or wider. The owner agrees to the drawing before any code (the Issue). Read beside [belt.md](belt.md) (sections 1 and 7), [spec.md](spec.md) (section 12, the read surface) and the runbook [herdr-app.md](../runbooks/herdr-app.md). A design review (one round) was folded in: the repository in every match and request (3.1, 4), the brief head in the approval (3.1), a failed run told from a suspended one (3.3), concluded pull request watches (3.3), approving the read that was shown (4), the language (2) and the truncated scan (3.1).

Two things are not decided here and stop the code: the drawing needs the owner's agreement, and where the screen runs needs a measurement on the owner's machine (section 5). A third is a boundary: the screen acting with the person key (section 4).

## 1. What the owner decided (2026-11-22)

Taken from the Issue's comments; they replace the first list in its body.

| Decision | In this note |
| --- | --- |
| The screen is where work is put in, watched and answered, in that order of weight. The conversation with the secretary is at its centre. | Section 2: three columns, the conversation in the middle and the widest. |
| Put in: the conversation; a range handed to the belt (start, pause, end); the next candidates with "ask for this"; every Issue that could go in now, and for the others why not. | Left column. |
| Watch: the belt (what flows, what stopped and why, the budget); the whole range as one list in a stable order; what went wrong; the runs, stopped ones first. One step further in: pull requests, the trail of one run. | Right column. |
| Answer: what waits for the person with its background, what each answer does and the recommendation; one entry large, several as a list. | Over the conversation. |
| Not in the first version: a notification that calls the person; the organization's own state (capacity, versions). | Left out. The dispatcher's heartbeat stays: a silent dispatcher is "what went wrong", not the organization's description. |
| herdr-app is the first candidate for where it runs, fixed only after a measurement. | Section 5. |

## 2. The layout

One screen, no pages to move between for the three roles. A top line and three columns.

| Part | What it holds | Why there |
| --- | --- | --- |
| Top line | The organization's name; "N waits for you"; "N went wrong"; the dispatcher's heartbeat and the age of the read. | The two counts are the only things that call for the person; they are readable from across the room and repeat nothing else. |
| Left, **Put in** | The range (its terms, the budget used, pause and end; a proposed range with the read to approve); "Could go in now"; "Cannot go in yet" with the reason of each. | Putting in is the most frequent act, and reading starts at the left. |
| Centre, **the conversation** | The secretary's terminal, as it is. Over it, **what waits for the person**. | Asking is done by talking to the secretary. A question arrives there too (belt.md section 1, "Answer"), so the entry and the conversation it came from are seen together. With nothing waiting, the conversation takes the whole column. |
| Right, **Watch** | "Went wrong" (only when there is something); the range, whole: a strip of one mark per Issue, the count per place, then every item; the runs outside a range, stopped ones first. At the foot, the two steps further in: pull requests, the trail of a run. | The person decides nothing here (the owner: its purpose is reassurance), so it sits at the side, always in view, and asks for no click. |

Rules of the look, which the drawing follows:

- A state is never colour alone: each place has a mark, a word and a colour (`✓ landed`, `? waits for you`, `● at work`, `◆ review`, `○ not yet in`, `– skipped`, `! failed`).
- Amber means "waits for the person" and nothing else. Red means "went wrong" and nothing else. Green is work that moves or landed. Everything else is ink on paper.
- An empty list says why it is empty (the panel "nothing could go in"): the scan found nothing, failed, or has not run.
- The range's list never reorders what has been admitted (belt.md 7.1); a row changes its mark, not its position.
- Every time is an age ("6 min") beside the newest read's age in the top line, so an old screen cannot pass for a fresh one. When the heartbeat is old the screen says so over everything and greys what it last read (the panel "the dispatcher is silent").
- Words are the person's, not the record's: "waits for you" for `waiting_person`, "not yet in" for `not_in`. The text is meant to follow `conversation_language` (spec section 13), but neither output prints the language in force (the setting, or `makibad`'s environment when unset): missing (section 6). Until then the screen has a setting of its own, `--language`, and falls back to the environment of its own process, read as spec section 13 reads it. The drawing is in English.

## 3. Each part and its read field

The screen opens no table and no file. It reads `makiba status --json` on an interval and `makiba events --json --after <id>` for the trail (spec section 12). "Today" means the field is on the read surface now; "missing" means the part cannot be drawn until the field exists, and section 6 lists those.

### 3.1 Put in

| Part | Read field | State |
| --- | --- | --- |
| The range: id, project, status, terms | `ranges[].id`, `.project`, `.status`, `.issues`, `.takes`, `.at_once`, `.approval_mode` | Today |
| Budget used | `ranges[].budget.tasks`, `.used.tasks` | Today (hours and fixes are belt step 2) |
| Approved when | `ranges[].approved_at` | Today |
| A proposed range and its read | `ranges[].candidates[]` (`issue`, `title`, `admitted`, `left_out_why`), `.read_id`, `.read_at`, `.read_complete` | Today |
| What every task of the range starts from | `ranges[].brief_head`, whole, never cut: the approval covers it (belt.md section 1, "Approve"), so a proposed range shows it in full, scrolling inside its box, before the button can be pressed | Today |
| Could go in now | `next_work.candidates[]`: `repo`, `issue`, `title`, `clause_id`, `size`, `size_estimated`, `why`, `recommended` | Today |
| Cannot go in yet | `next_work.left_out[]`: `repo`, `issue`, `kind`, `reason`; `candidates_not_recorded` and `left_out_not_recorded`, both numbers, shown as "and N more not listed" | Today (a left-out entry has no title: missing) |
| The scan read only part | `next_work.input_truncated`, a list of the reads cut short (`open_issues`, `open_prs`, `base_merges`), not a count: shown as its own warning, "the scan did not read every open Issue", naming the reads | Today |
| Why the list is empty | `next_work` null (no scan yet), `.status` (`no_candidates`, `error`), `.error`, `.scanned_at`, `.trigger` | Today |

While a range is active the list is headed "outside the range" (belt.md 7.1). An Issue the range would admit is shown once, in the range. The match is by repository and number, never by number alone: a range's repository is `projects[].repo` of its `ranges[].project`, and a candidate carries `next_work.candidates[].repo`. With more than one project registered every reference on the screen is written `OWNER/NAME#N`; with one, `#N` (as the drawing).

One point where the owner's words and the record differ: the owner lists "matching no goal clause" among the reasons an Issue cannot go in. The scan does not leave such an Issue out: it is a candidate with `clause_id` null, ranked after the ones that name a clause. The drawing follows the record (a candidate tagged "no clause", ranked last). If the owner wants it under "cannot go in yet", that is a change to the scan, not to the screen.

### 3.2 Answer

| Part | Read field | State |
| --- | --- | --- |
| The entry: number, task, age, question | `pending[].id`, `.task_id`, `.created_at`, `.question`, `.status` | Today |
| Background, what each answer does, the recommendation and why | `pending[].context.background`, `.if_yes`, `.if_no`, `.recommend`, `.why` | Today |
| Which answer is the recommended one (the highlighted box) | `ranges[].stops[].recommend_answer`, by the entry's id | Today for a range's stop; for other entries the recommendation is shown as text only |
| Its range and stop line | `pending[].stop_line`, `.to` | Missing (belt step 5); until then, from `ranges[].stops[]` |
| A permission prompt | `pending[].kind`; first line only, no context (the read surface shows no more) | Today; the row says "a permission prompt, no context recorded" |

One entry is shown large. Several are a list, oldest first, and the one opened is shown large. Entries `open` (the secretary has not escalated them) are not the person's yet: the screen shows only `escalated` ones as waiting, and counts the open ones in one quiet line ("2 with the secretary").

### 3.3 Watch

| Part | Read field | State |
| --- | --- | --- |
| The range, whole: one row per Issue with its place, stage, reason, age, pull request | `ranges[].items[]`: `issue`, `title`, `place`, `stage`, `why`, `since`, `task_id`, `pr`, `pr_title` | Today |
| The strip and the counts | `ranges[].places` | Today |
| Skipped, and why | `ranges[].skipped[]` | Today |
| An ended range | `ranges[].status`, `.ended_reason`; its list stays while it is the project's newest (belt.md 7.1) | Today |
| Went wrong: an open anomaly | `anomalies[]`: `task_id`, `rule`, `since` | Today |
| Went wrong: a failed launch | `runs[].status` `failed`, never the stage alone: `makiba down` sets a launch's stage to `failed` while it suspends the run, and writes no `worker_spawn_failed`. A run `suspended` is shown as "suspended" under the runs, not as gone wrong. The reason is shown only from a `worker_spawn_failed` event whose `spawn_id` is the run's newest launch; an older launch's failure is never attached | The status today. The reason is missing: `runs[]` does not print the newest launch's `spawn_id` or its reason (section 6); until then the row says "launch failed" with no reason |
| Went wrong: the dispatcher is silent | `makibad.running`, `.heartbeat_at` | Today |
| Went wrong: the last scan failed | `next_work.status` `error`, `.error` | Today |
| Runs outside a range, stopped first | `runs[]`: `task_id`, `kind`, `project`, `status`, `stage`; the worker's state and age from `terminals[]` by the name `w-<task_id>` | Today, without a title and without "since" for the run itself: missing |
| Order "stopped first" | Failed launch, then an anomaly, then `waiting_person`, then a terminal `blocked` or `idle`, then the rest by age | Computed by the screen from the fields above |
| Closed today | Events `run_closed` since local midnight | Today, from the events |
| Pull requests: the heads CI is running on | `pr_watches[]`: `repo`, `pr`, `head_oid`, `task_id`, `started_at`. Only watches still `watching` are listed: a watch leaves the list the moment CI concludes | Today |
| Pull requests: what CI concluded | Not on `status`. The panel keeps its own list from the events: a `pr` event (`opened`, `pushed`) adds or replaces a row by `repo` and `pr`; `ci_completed` of the same `head_oid` sets its conclusion (`status`, `fail_count`, `total_checks`); `pr_merged` marks it merged; `run_closed` of its task with no merge removes it | Today from the events, as a stopgap; missing as a field (section 6), and built into the first version only with that field |
| The trail of one run | Events with that `task_id`, oldest first | Today; the screen keeps the events it has read, since `events` has no filter by task (section 6) |

The trail names each event in the person's words (`belt_dispatched`: "Dispatched by makibad"; `worker_spawn_verified`: "Launch verified"; `belt_stopped`: "Stopped for you"). A kind with no wording is shown by its name, never dropped.

### 3.4 The conversation

In herdr-app the centre is the secretary's terminal itself, which the app already shows by its labels (probe P12). The screen adds nothing to it and reads nothing from it. Its state and age in the bar over it come from `terminals[]` (`role` `secretary`: `state`, `since`).

In a page on 127.0.0.1 there is no terminal: the read surface shows no message body, by design. What a page could hold instead is in section 5.

## 4. Acting from the screen

Reading needs no key. Every act is a verb of the door, sent as the person's `makiba` sends it; the screen adds no verb and no way around one.

| Control | Verb | Key | Note |
| --- | --- | --- | --- |
| Yes / No, with a note | `answer <pending id> yes\|no [--note]` | Person | `stale` or `conflict` is shown in the entry; nothing is retried. |
| Talk it over first | none | none | Moves the focus to the conversation. |
| Approve this read | The door's `range` `approve` with `read` set to the `ranges[].read_id` the screen showed | Person | A newer read is `stale`: the screen shows the new read and asks again. The person's command cannot say which read: `makiba range approve <id>` fetches the newest read itself, so with `--yes` it could approve a read the screen never showed. The screen therefore sends the door request, or the command gets a `--read <id>` argument first (part of the boundary design below). The read-only screen's line to type is `makiba range approve <id>` without `--yes`: the command prints its read and asks. |
| Pause | `range pause <id> --reason` | Person | The reason is asked for; "paused from the screen" when left empty. |
| Resume | `range resume <id>` | Person | |
| End | `range end <id>` | Person | Asks once: work already running goes on to its end. |
| Ask for this | `send secretary "<text>"` | Person | A fixed sentence naming the repository and the Issue: "Please take up OWNER/NAME#N (<title>)." The secretary decides how; the screen dispatches nothing. |

"Ask for this" is a message and not a `dispatch` so that the secretary stays the one contact (goal G1) and the brief stays the secretary's work. A new range is drafted in the conversation (belt.md section 1); the screen has no form for one.

This is a trust boundary, and it is designed separately before any of it is built: a program other than the person's shell would send requests with the person key. The questions that design must answer: who holds the key (the screen's own process reading `person.key` as `makiba` does, or the screen running the `makiba` binary); how a page's requests are told from another local program's (a page on 127.0.0.1 can be asked by anything on the machine, an agent included, unless its sandbox has no route there); and that a click is recorded as the person's act through the screen (`commands.actor`). Until that design is agreed, the screen is read-only and each control shows the line to type instead (`makiba answer 14 yes`), which the person runs in their shell.

## 5. Where it runs

Not decided. The owner's first candidate is herdr-app, because the secretary's terminal sits in the middle as it is. Whether the app can read makiba's read surface has not been measured, and this note does not guess how the app is extended.

| | In herdr-app | A page on 127.0.0.1 |
| --- | --- | --- |
| The conversation at the centre | The terminal itself (P12). | Not there. A text box that sends to the secretary gives no answer back, since no message body is on the read surface; the person keeps a terminal beside the page. |
| Reading `status --json` and `events --json` | Not measured (M1 below). | A process the person starts (`makiba screen`) reads them as `makiba status` does and serves the page. |
| Acting with the person key | Not measured (M2). | The process holds the key; the questions of section 4. |
| What makiba ships | Whatever the app loads; not known. | Part of the one binary (goal G10). |
| Works when the app is not installed | No. | Yes. |

The measurement, to be run by the owner on the machine where the app runs, with a real organization up. Each row is a yes or no with the version of `hc` and of the app, recorded in [probes.md](probes.md) as a new probe:

| # | Question | Passes when |
| --- | --- | --- |
| M1 | Can something the app shows, on the machine registered for makiba's session, obtain the output of `makiba status --json`? By which way: running the command on that machine, or a request to 127.0.0.1 of the base's host. | The JSON of a running organization is displayed inside the app, refreshed without the person's hand. |
| M2 | Can it run `makiba answer <id> yes` as the person, outside any agent's sandbox, and show the result? | The entry becomes `answered` and `commands` records the person as the actor. |
| M3 | Can the app show a page served on 127.0.0.1 of the base's host beside a terminal? | The page is displayed beside the secretary's terminal and refreshes. |
| M4 | What an addition to the app is made of and how it is installed, in one paragraph. | Written down. |

What follows from it:

- M3 passes: one thing is built, the page; the app frames it beside the terminal. This is the recommended outcome, since the page also works without the app.
- M1 passes and M3 does not: the screen is built for the app; the page is not built.
- Neither passes: the page, with the person's terminal beside it, and the centre column is given to "what waits for you" and the range.

Either way the parts and their read fields (section 3) are the same, so the model under the screen is built once: a function from the two JSON outputs to what each part shows (the order "stopped first", the wording of each event, the reason a list is empty). It is plain code with no place attached and is what the declared test covers.

## 6. What the read surface lacks

Each is a small addition to `status --json` with a row in the table `Status field` and in `tests/read_surface.rs`; each would be its own Issue, before or with the code of the screen. None blocks the drawing.

| Field | For | Without it |
| --- | --- | --- |
| `runs[].title`, `runs[].since`, `runs[].issue`, `runs[].range_id` | "Other runs" and the trail's heading | A run outside a range is shown by its task id only, with its terminal's age. |
| `next_work.left_out[].title` | "Cannot go in yet" | The row shows the number and the reason. |
| `pending[].stop_line`, `.to`, and the `belt[]` rows (belt.md section 7, step 5) | The entry's range; what stopped and why, per task | Read from `ranges[].stops[]` and `ranges[].items[]`, which cover a first version. |
| `pull_requests[]`: every pull request of a run not closed, with `repo`, `pr`, `head_oid`, `task_id`, `ci` (`running`, or the newest conclusion at that head) | Pull requests | `pr_watches[]` drops a watch when CI concludes, so the conclusion is folded from `pr`, `ci_completed` and `pr_merged` events by the screen. |
| `runs[].launch_failed_reason` (the newest launch's `failed_reason`) | Went wrong | The row says "launch failed" with no reason: without the newest launch's `spawn_id` an event cannot be tied to it. |
| `conversation_language`, as resolved | Every label | The screen's own `--language`, else its process's environment. |
| `events --task <task_id>` | The trail of an old run | The screen keeps what it has read; a run older than its memory is read again from event 0. |
| `closed_today[]`, or runs closed since a time | "Closed today" | Folded from `run_closed`. |

The rule behind the list: the screen states nothing `makibad` could state. Folding events in the screen is accepted for the trail, which is the events, and is a stopgap everywhere else.

## 7. The order of building

Nothing starts before the owner agrees to the drawing.

| Step | What | Needs | Declared test |
| --- | --- | --- | --- |
| 0 | The drawing agreed; the measurement of section 5 run and recorded. | The owner | none (documents) |
| 1 | The model: from `status --json` and `events --json` to the parts of section 3. Read-only. | Step 0; the read surface as it is | `tests/screen_model.rs`: from a recorded status with one escalated entry, one failed launch and a range of mixed places, the model gives the entry with its context, "went wrong" with the launch's reason, the range's rows in the read's order, and the runs with the stopped ones first; from a status with `next_work` null, in error and with no candidates, three different reasons for the empty list. |
| 2 | The frame chosen in step 0 (the page behind `makiba screen`, or the app's), read-only, each control showing the line to type. | Step 1 | Depends on the frame; for the page, a test that it answers on 127.0.0.1 only and serves what the model gives. |
| 3 | The missing read fields of section 6, each its own Issue. | - | `tests/read_surface.rs` grows. |
| 4 | Acting from the screen (section 4), after its own design and the owner's agreement. | M6-1; the boundary design | Its own. |

## 8. Not in the first version

A notification that calls the person (Issue 272). The organization's own state: capacity, the base, the versions (the owner, 2026-11-22). The screens of workers (the person is never called to a terminal; `peek` stays the secretary's). Several organizations or several people on one screen (belt.md section 10: the fields allow it, the layout does not try). A phone-sized layout.
