# The belt: work inside an approved range

Status: design note for Issue 248, 2026-11-21, for the owner to decide on. Nothing here is built; no name below is in `src/spec.rs` yet. Read beside [plan.md](plan.md) (R4, M7), [spec.md](spec.md) (sections 5, 10, 12 and the read surface) and the skill `skills/conveyor/SKILL.md` (Issue 188), which this note turns from a skill into records. A design review (one round, before the commit) was folded in; what it found is marked "review" where it changed the design.

Today the person is the clock: a run closes, `makibad` scans, the secretary proposes, the person picks. The conveyor skill lets the secretary keep going inside a scope, but the scope lives in the conversation, the count of slots is the secretary's own, and every stop holds a slot. On 2026-11-21 claude-org-ja ran this belt by hand over about a dozen makiba Issues; this note says what of that becomes a record or a verb here.

## 1. What the person does

| Step | What the person does | What they see |
| --- | --- | --- |
| Approve | The secretary proposes a range in the conversation: which Issues (a list, a condition, or both), how many at once, how much time and how many tasks, how follow-ups found on the way are treated, the text every task starts from, and the merge approval mode in force. The person approves it with their own key (one command, or a button on the screen of Issue 169). | The range as `makibad` stored it, read back. |
| Watch | Nothing. Tasks go in as slots free up, without a call. | The range, each task on the belt and where it stands, the budget used. |
| Answer | When something on the belt needs them, they get one question in the conversation, with a recommendation. While it waits, the other tasks go on and the free slot is filled. | Each stopped task with its line, its reason and how long it has waited. |
| Pause | Say pause (or press it). Nothing new goes in; work already running goes on. Resume with their key. | The range as paused. |
| End | Say end, or let it run out (budget spent, nothing left inside). Work already running finishes; nothing is abandoned. | The range as ended, with why. |

## 2. The range as a record

### 2.1 Fields

| Field | Meaning | Who evaluates it |
| --- | --- | --- |
| `id` | Integer, from `makibad`. | - |
| `project` | One registered project (its slug). One active range per project (Q7). | - |
| `issues` | An explicit list of Issue numbers, or empty. | `makibad` |
| `takes` | A condition over open Issues, or empty. A fixed grammar, joined by AND: `label:<l>`, `no-label:<l>`, `milestone:<m>`, `clause:<=G<n>` (the goal clause the Issue names, `docs/goals.md`). A candidate is in the range when it is in `issues` or matches `takes`. | `makibad`, on its own read of the project's open Issues (section 3.3). No model. |
| always | Every candidate must also be open; have no run of its own not `completed` or `abandoned` (by `runs.issue_refs`, which a range dispatch fills); have no open pull request that closes it; and have every Issue named on a `Depends on` line of its body closed. A dependency whose state cannot be read counts as open. | `makibad` |
| `at_once` | Runs of the range that count at a time (section 4). Never more than `max_concurrent_workers`. | `makibad` |
| `budget_tasks` | Tasks the range may dispatch in all, follow-ups included. Taken at dispatch, never given back. Required. | `makibad` |
| `budget_hours` | Wall-clock hours from approval. Optional. | `makibad` |
| `budget_fixes` | Heads recorded with `pr pushed` for a task's pull request after the first. Default 3. A stop of that task, not a guard on the push. | `makibad` |
| `follow_ups` | `inside` or `ask`. `inside`: an Issue created after approval in the same repository, whose body has a line `Follow-up of #<n>` naming an Issue of the range, is in the range (and takes from `budget_tasks`). `ask`: it is not, until a new range is approved. | `makibad` |
| `design_ahead` | `yes` or `no`. `yes`: a candidate whose only open dependencies are tasks of this range with a pull request may be dispatched for its design only (section 4.3). | `makibad` |
| `brief_head` | The text every brief of the range starts with (conventions, verification depth, what to stop on), written in the proposal and approved with it. | - |
| `approval_mode` | `merge_approval_mode` as `makibad` read it at approval, the way `gate` reads it. | `makibad` |
| `proposed_by`, `proposed_at` | The proposer (the secretary or the person). | `makibad` |
| `approved_by`, `approved_at` | Always `person`, with the time. | `makibad` |
| `status` | `proposed`, `active`, `paused`, `ended`. | `makibad` |
| `ended_reason` | `person`, `secretary`, `budget_tasks`, `budget_hours`, `used_up`, `scan_error`, `replaced`. | `makibad` |

The condition and `brief_head` must be enough to admit a task with nobody looking (review): `makibad` dispatches in the same step a slot frees, so a candidate that needs a person's or the secretary's look first belongs in an explicit list, not in `takes`. The secretary may take a candidate out before it is dispatched (`skip`, with a reason, recorded); it can never put one in. What could only be judged by reading (on 2026-11-21: "the body asks for no decision of the person first") is the secretary's `skip`, or the proposal's list.

### 2.2 Where it is stored

| Store | Holds |
| --- | --- |
| `state.db`, table `ranges` (migration v8) | One row per range: the fields above. |
| `state.db`, table `range_candidates` (migration v8) | The newest read of the range's candidates: Issue number, title, body, comments, labels, `Depends on` numbers, closing pull request, read time. What a dispatch's brief is made from. |
| `state.db`, `runs.range_id` (migration v8), `runs.issue_refs` (exists) | The range a run was dispatched under (null outside every range), and its Issue. |
| `state.db`, `events` | `range_proposed`, `range_approved`, `range_paused`, `range_resumed`, `range_ended`, `range_read`, `belt_dispatched`, `belt_skipped`, `belt_stopped`, `run_parked`, `run_unparked` (section 7). |
| `state.db`, `pending.context_json` | `stop_line` on an entry that is a stop of a range task. |

Why not a file (claude-org-ja keeps `.state/conveyor/scope-contract.md`):

- A file the secretary can write is a range the secretary can widen. `state.db` is written by `makibad` only (spec section 1), and no agent role can write it.
- The counts (tasks taken, slots taken) must move in the same transaction as the dispatch, the escalation and the close that change them.
- A renewed secretary reads the range from `status`; the handover no longer has to carry it whole (the skill's section 6 today).
- The screen of Issue 169 reads `makiba status --json` and `makiba events --json`, never a file.

### 2.3 Who may write it

| Verb (`range ...`, new) | Secretary | Person | Effect |
| --- | --- | --- | --- |
| `propose` | yes | yes | A `proposed` row. Changes nothing else. |
| `approve <id>` | no | yes (person key) | `proposed` to `active`; `approval_mode`, `approved_at` written; a read and the first dispatches follow (section 3). |
| `pause <id>` | yes | yes | No new dispatch. Narrowing, so the secretary may. |
| `resume <id>` | no | yes | `paused` to `active`. |
| `end <id>` | yes | yes | `ended`, reason `secretary` or `person`. |
| `skip <id> <issue> --reason` | yes | yes | Takes one candidate out of the range before it is dispatched (`belt_skipped`). |

There is no verb that changes an active range. Widening anything (more Issues, budget, `at_once`, `follow_ups` from `ask` to `inside`) is a new proposal, approved again; approving it ends the old one (`replaced`) in the same transaction, and the old range's runs move to the new one.

What the range does not bound (review): a `dispatch` that names no range. That is today's path, the secretary acting on the person's word in the conversation, and it stays as trusted as today. It is not silent: such a run counts against `max_concurrent_workers`, so it takes from the belt's room, and the read surface shows it as a run with no range. Whether a dispatch outside a range should need the person's key is Q8.

### 2.4 How it ends, and how it pauses

| Transition | Cause | By |
| --- | --- | --- |
| `active` to `ended` | `budget_tasks` all taken and every task of the range closed; `budget_hours` past; `used_up` (a complete read finds no candidate and no task of the range is open); `scan_error` (the read reports the project unset or in error, never read as "no candidates"). | `makibad` |
| `active` or `paused` to `ended` | `end`, or a replacing approval. | Secretary or person |
| `active` to `paused` | `pause`; two launches of the range failed in a row (a verified launch resets the count); two refusals by the gate of different pull requests in a row (a merged one resets it). | Secretary, person, `makibad` |
| `paused` to `active` | `resume`. | Person |

Once `budget_tasks` are all taken or `budget_hours` is past, nothing new is dispatched; the range shows as ending until its last task closes. In every case runs already dispatched go on to their own end: the secretary still pushes, merges per the mode and closes them. Nothing is abandoned by an ending.

## 3. The loop

### 3.1 Who holds it: `makibad`

The rule the owner cares about: whoever holds the loop is outside the actor it restrains. The range restrains the secretary (what may go in without asking, how much). So the secretary must not be the one who decides that a candidate is inside, that a slot is free or that the budget allows one more. `makibad` holds the clock, the count and the dispatch; the secretary keeps the judgment (answering workers, the pull request, the merge under the mode, the close, the stop lines that are judgment).

`makibad` dispatches the next task itself (recommended, Q2): a run with the range's `brief_head`, then the Issue's title, body and comments from `range_candidates`. That also ends copying Issue text to workers by hand. The task id is `r<range id>-<issue>`, so a task is dispatched once whatever restarts. The secretary may still dispatch a range candidate with a brief of its own, naming the range; `makibad` refuses it when the Issue is outside the range or no slot or budget is left.

### 3.2 The count

A slot is free when both of these are below their limit:

| Count | Counts | Does not count |
| --- | --- | --- |
| The range's, against `at_once` | Its runs `queued` (from the moment of dispatch, so one transaction cannot admit more than `at_once`; review) or `in_use`. | Runs waiting only for the person (4.1), `review`, `parked`, closed. |
| `makibad`'s own (`dispatch::busy`), against `max_concurrent_workers` | As today: runs `in_use`, and `queued` or `suspended` with a launch under way. | Changed (review): runs waiting only for the person, so that a stop frees a launch with one slot in all. |

A run that comes back (answered, unparked, sent back to rework) counts again at once, even above the limit; nothing new goes in until the count is below it.

### 3.3 Reading the candidates

The triage pass cannot serve the clock as it is (review): it scans only after a close, at most every 30 minutes, keeps the top 3 before any range is applied, and keeps no Issue text. So the range has its own read: on approval, on `resume`, after each close of a range task, and every 10 minutes while the range is active (a first proposal), `makibad` reads the project's open Issues whole (body, comments, labels, milestone) and the open pull requests with the Issues they close, outside any transaction. It applies the range to every Issue before any cut, looks up the state of each dependency not in the read, and writes `range_candidates` and a `range_read` event in one transaction. A read that fails keeps the previous one and is said in the log; only a complete read can end a range `used_up`.

### 3.4 State table

"Next candidate" is the first row of `range_candidates` in triage order (goal clause, then the carried order) that the records still allow (no run of its own, a slot, budget) and that does not depend on a task waiting for the person (4.2).

| Event | `makibad` does | The secretary does |
| --- | --- | --- |
| The person approves a range | Reads, then dispatches next candidates until no slot is free (each `belt_dispatched`). | Tells the person what went in. |
| A slot is free (any cause below) | In the transaction of the event that freed it: dispatches the next candidate from the newest read. With none, and no task of the range open after a complete read: ends it `used_up`. | Nothing. |
| A worker reports `done` | Run to `review` (as today); it stops counting. Slot free. | Push, pull request, `pr` (skill `pull-request`). |
| CI failed | Nothing new. | Sends the worker back. |
| The worker starts the rework | `report start` from a worker at `review` moves its run to `in_use` (new, review): it counts again. | Nothing. |
| A new head is recorded (`pr pushed`) | At the `budget_fixes`-th head after the first: a stop of the task, line budget (section 5). | - |
| CI passed | Nothing new. | Merges under the mode and the stop lines, or puts the stop to the person. |
| The gate refuses | A stop of the task, line gate (section 5). | Tells the person, with the link. |
| A run closes | Reads again; slot free if it counted; checks the budget. | Closes after the merge (as today). |
| A worker asks | Nothing yet. | Answers what it can. What is the person's: `pending add` and `escalate`. |
| An entry is escalated to the person | The run stops counting; slot free, in the escalate's transaction (4.1). | Puts the question to the person. |
| The person answers | The run counts again from the answer. | Carries the answer (`answer`). |
| A launch of the range fails | Counts failures in a row; at two, pauses the range and tells the secretary. | Tells the person. |
| Budget taken or hours past | No new dispatch; the range ends when its last task closes. | Tells the person; open tasks go on. |
| The person or the secretary pauses | No new dispatch. | Says so; open tasks go on. |
| The secretary is renewed | Nothing: the range is in the records. The resume brief's fixed line tells the secretary to read `status` first, which shows every range with a live run. | Reads it. |
| `makibad` restarts | Derives everything from the records on its next pass; a task id is dispatched once, so nothing goes in twice. | Nothing. |

## 4. The owner's requirement: a stop holds a task, never a slot

### 4.1 Rules and their mechanism

| Rule | Mechanism |
| --- | --- |
| A run waits only for the person when it has a pending entry `escalated` (a question or a stop line), or it is at `review` with its pull request stopped at a stop line or waiting for the person's merge. | Records: `pending.status`, `runs.status`, `belt_stopped`. |
| Such a run counts neither against `at_once` nor against `max_concurrent_workers`. | Both counts of 3.2 leave it out. The skill's own count (every run not closed) is dropped. |
| The question and the filling of the free slot happen in one step. | The `pending escalate` transaction also dispatches the next candidate from the newest read (3.4). No turn of the secretary or the person is between them. |
| Its terminal can be let go (step 3, section 8). | A run that waits only for the person on a question (never on a permission prompt; review), and whose terminal has not been `working` for 120 s, is parked: `makibad` ends the terminal, revokes its key, keeps the workplace and the conversation id, and sets the run `parked` (`run_parked`). A prompt's answer needs the terminal that showed it (spec section 7), so a run stopped on a prompt keeps its terminal and still frees its slot. |
| And brought back. | `answer` on its entry, a `send` to it, or the landing of what it waits for (4.3) unparks it: a relaunch by resume, the path `makiba up` uses for a `suspended` run (Issue 41), with the answer or message delivered by the box once its channel connects (`run_unparked`). `parked` is a status of its own so that `makiba up` does not relaunch it. |
| An answered run comes back before a new candidate goes in. | Unparking waits for no slot (3.2). |

### 4.2 What "does not depend on the answer" means

A candidate depends on a task waiting for the person when a `Depends on` line of its body names that task's Issue. `makibad` passes over such a candidate while the task waits and takes the next. Only the body's `Depends on` lines are read as dependencies (not comments, not other words; review: the triage parser reads more, and the range uses this one grammar). A dependency the Issue does not write is the secretary's `skip`.

### 4.3 A task waiting for another may still do its design

This is for a dependency that waits for its landing, not for an answer (4.2 is the other case). With `design_ahead: yes`, a candidate whose open dependencies are all range tasks with a pull request is dispatched with one line added to its brief: design only, stop and report before any code, because `#<n>` (each listed) has not landed. Its run records the set it waits for. When it reports, it is parked: it waits for a landing, not a person, and does not count. When every task of the set has closed `completed`, `makibad` unparks it with one message naming each landed Issue and main's head: update the branch and build. A dependency closed `abandoned` is a stop of the waiting task for the person (line 1).

## 5. The stop lines in makiba's terms

Each stop of one task is a pending entry for the person with `stop_line` set, and a `belt_stopped` event; the belt goes on. `makibad` writes both when a record decides the line; the secretary writes them (`pending add` with the line, then `escalate`) when it is judgment.

| Line | Detected by | Held today? |
| --- | --- | --- |
| 1. A decision that is the person's | The secretary's `escalate` (judgment; the record makes it visible and frees the slot). Skill `pull-request` line 3 for a pull request. | Judgment only. |
| 2. A change outside the range | A candidate outside the range: never dispatched by `makibad`, and refused when the secretary names the range (code). A dispatch naming no range: not bounded (2.3, Q8). A change beyond its brief inside a task: judgment (skill line 3). | Not yet: no range record. |
| 3. An irreversible action | Skill line 4 (judgment). Code holds parts: `push` is never forced, `down` and the base are the person's, and a change with no declared test merges under `acceptance` only when it is Markdown or under `docs/`. Not held: a release tag, a change of settings or access made through `gh`. | Parts, by `push` and `gate`. |
| 4. The budget | `makibad` counts `budget_tasks`, `budget_hours` (whole range) and `budget_fixes` (one task) from records. | No. |
| 5. A suspicion that a test was rewritten to pass | `gate`: the test commit is the first and unchanged, the declared paths and test setup at head are as at the test commit, the manifest digest is pinned. Not held: an edit to an existing test outside the declared paths (on 2026-11-21 every such pull request was landed by the owner by hand). Proposed for step 2: `gate` reports a change to an existing file under `tests/` not declared, and the belt makes it a stop. | Yes for declared tests. |
| 6. A refusal by the gate | Every `gate` answer with `allow` false (review: `rejected`, and `no_manifest` refused under `acceptance`), including the refusals decided before the git checks (a head or CI mismatch), which then also write an `acceptance_gate` row so that a record exists. One stop per pull request and head. | The refusal, yes; the stop record, no. |
| (mode) A range approved under `human` | Judgment, as today: the secretary merges without asking only when the range's `approval_mode` and the mode read now are both `acceptance` (skill `conveyor`). Review: substituting `human` in the gate would not hold it, since the gate under `human` refuses less, and no record of the person's merge word exists for the gate to read. | Skill only. |

Today's acceptance gate (Issue 47) already holds: the head and passed CI must match, the declared test failed before and passes at head, the test and its setup are unchanged since the test commit, a pull request without a declared test merges under `acceptance` only for docs, and the merge-gate hook refuses every other way to merge from a command line.

## 6. What was done by hand on 2026-11-21, and what replaces it

| Hand work (today.md) | Here |
| --- | --- |
| Writing the scope contract in a file and appending each widening | Record: `ranges`, `range_*` events; widening is a new proposal. |
| The owner's "OK" in chat as the approval | Stays with the person, as their key (Q1). |
| Picking the next Issue whose `Depends on` are closed | Record and code: the range's read and order. |
| Copying Issue text and comments into the worker's scratch folder | Record and verb: `range_candidates`, put into the brief by the range's dispatch. |
| Counting panes as slots; a pane waiting for a landing held a slot for hours | Record: the counts of 3.2; parking. |
| Leaving free slots empty while a follow-up was asked one by one | Record: `follow_ups: inside`. |
| A dependent task not started, not even its design | Record: `design_ahead`. |
| Running the whole suite outside the sandbox before a push | Not needed: CI (with the real-base job) is the evidence the gate reads; a worker says how it ran its change. |
| Bringing a branch up to date and restarting the watch | Already `makibad` (`update`); the secretary records the new head with `pr pushed`. Folding that into `update` is a small follow-up. |
| Watching CI with a pane per pull request | Already `makibad` (PR watches). |
| Closing the terminal and removing the workplace | Already `makibad` (`close`). |
| Re-dispatching after a host restart | Already `makibad` (`up` relaunches suspended runs). |
| Filing follow-up Issues | Stays with the secretary (skill `issue`); the range decides whether they are inside. |
| Asking the same question again after silence | Record: each stop with its age on the read surface and the screen. Nothing reminds the person today: `secretary_still` watches only entries still `open` (the secretary's own delay), not `escalated` ones. A notice to the person is stage 2. |
| Answering workers' questions "all recommended" on the owner's standing word | Stays with the person (Q4): not in the first version. |
| The order of landings to avoid repeated merges of main | Stays with the secretary. |
| Landing on the owner's screen (CI or script changes, edited tests, no declared test) | Stays with the person: stop lines 3 and 5. |
| Setting the review round cap per task | Not carried yet (L-K10, Issue 135); `budget_fixes` counts heads meanwhile. |
| Host runs and probes | Stays with the person or the secretary outside the belt; a task that needs one is a stop (line 1). |
| Re-running a flaky CI job | Stays with the secretary. |
| A task list (`TaskList`) as the belt's display | Record: the read fields of section 7. |

## 7. Read fields for the screen of Issue 169

Additions to the table `Status field` (spec section 12). The `ranges` rows ship with step 1, since a renewed secretary reads the range there (review); the `belt` rows with step 5.

| Status field | Source |
| --- | --- |
| `ranges[].id`, `.project`, `.status`, `.ended_reason` | `ranges`: every range not `ended`, and every ended range with a run not closed |
| `ranges[].issues`, `.takes`, `.brief_head` | `ranges`, as approved |
| `ranges[].at_once`, `.follow_ups`, `.design_ahead`, `.approval_mode` | `ranges` |
| `ranges[].budget.tasks`, `.hours`, `.fixes` | `ranges.budget_*` |
| `ranges[].used.tasks`, `.hours` | runs with the range's `range_id`; hours since `approved_at` |
| `ranges[].skipped[]` | `belt_skipped` of the range: `issue`, `reason` |
| `ranges[].read_at` | the newest `range_read` |
| `ranges[].approved_by`, `.approved_at` | `ranges` |
| `belt[].range_id`, `.task_id`, `.issue` | runs with a `range_id`, not `completed` or `abandoned` |
| `belt[].place` | `launching`, `at_work`, `waiting_person`, `parked`, `review`, `stopped`, from `runs.status`, the newest launch's stage and the run's open entry |
| `belt[].counts` | whether it counts against `at_once` (3.2) |
| `belt[].stop_line`, `.stop_reason`, `.pending_id` | the newest `belt_stopped` of the run while its entry is open; the reason's first line |
| `belt[].waits_for` | the Issues a design-ahead run waits for |
| `belt[].since` | when its place began |
| `pending[].stop_line` | `pending.context_json.stop_line` |
| `runs[].range_id` | `runs.range_id` |

The table `Event field` does not change; the new kinds go into spec section 10 with their payloads:

| Kind | Payload |
| --- | --- |
| `range_proposed`, `range_approved`, `range_paused`, `range_resumed` | `range_id`, `reason` (for a pause) |
| `range_ended` | `range_id`, `reason` |
| `range_read` | `range_id`, `candidate_refs`, `complete` |
| `belt_dispatched` | `range_id`, `issue`, `task_id`, `design_only` |
| `belt_skipped` | `range_id`, `issue`, `reason` |
| `belt_stopped` | `range_id`, `task_id`, `line`, `reason`, `pending_id` |
| `run_parked`, `run_unparked` | `task_id`, `reason` |

## 8. Not in the first version, and the order of building

Not in the first version: parking terminals, `design_ahead`, `follow_ups: inside` (always `ask`), `budget_fixes` and `budget_hours`, the stops `makibad` writes itself, the gate's check of edited tests, the `belt` read fields, the screen.

| Step | What | Declared test |
| --- | --- | --- |
| 1 | The range record (`ranges`, `range_candidates`, `runs.range_id`, `range propose/approve/pause/resume/end/skip`, `at_once`, `budget_tasks`), its read (3.3), `makibad` dispatching the next in-range candidate itself when a slot frees, an escalated entry counting in neither count, and the `ranges` read fields. This alone removes the person as the clock. | `tests/belt.rs`: with a range approved for two tasks and one slot (`max_concurrent_workers` 1), the second is dispatched when the first closes with no call by the person; a task outside the range is not dispatched; an escalated stop leaves a pending entry and the next task is dispatched. |
| 2 | Stops written by code: every gate refusal and the budgets as `belt_stopped` with a pending entry; `budget_hours`, `budget_fixes`; rework counting again; launch failures and gate refusals pausing the range; the gate reporting edited tests. | `tests/belt_stops.rs`: a refused gate verdict on a range task leaves a pending entry with `stop_line` `gate` and the next task is dispatched. |
| 3 | Parking and unparking terminals; `parked` status; `up` leaves parked runs alone. | `tests/belt_park.rs`: a run waiting on an escalated question has no terminal after the wait, and `answer` brings it back on its conversation. |
| 4 | `design_ahead` and `follow_ups: inside`. | `tests/belt_ahead.rs`: a dependent candidate is dispatched design-only and unparked when its dependency closes completed. |
| 5 | The `belt` read fields, then the screen (Issue 169). | `tests/read_surface.rs` grows the rows above. |
| - | The skill `conveyor` shrinks to the judgment that stays with the secretary, in the pull request of each step. | `tests/conveyor_skill.rs`. |

## 9. Open questions for the owner

| # | Question | Options | Recommendation |
| --- | --- | --- | --- |
| Q1 | How does the person approve a range? | (a) Only with the person key: `makiba range approve <id>` in their shell, later a button on the screen. (b) The secretary records the approval, quoting the person's words. | (a). (b) lets an agent widen the range, which the Issue forbids. Cost: no approval from the phone until the screen or stage 3; pause and end still work from the phone through the secretary. |
| Q2 | Who dispatches the next task? | (A) `makibad`, with the approved `brief_head` and the Issue text. (B) `makibad` sends the secretary a notice and refuses a dispatch outside the range; the secretary writes the brief. | (A). The clock then holds without a model, survives a renewal and is testable. (B) keeps the secretary as the clock, and a judgment before each admission. A brief of the secretary's own for one Issue stays possible inside the range. |
| Q3 | What is a follow-up, and is it inside by default? | (a) `inside`: Issues created after approval in the same repository with `Follow-up of #<n>` naming a range Issue, taking from `budget_tasks`. (b) Always ask. | (a) as the default of a proposal from step 4, (b) until then. The task budget bounds what the secretary could add by filing Issues. |
| Q4 | May the range carry the standing "answer with your recommendation" of 2026-11-21? | (a) No: a decision of the person is always a stop. (b) A field `answers: recommend`, each such answer recorded as answered by the secretary and listed for the person. | (a) for now. (b) is a later widening, decided after a week of M7. |
| Q5 | Is parking needed in the first version? | (a) Step 3, after the counts. (b) In step 1. | (a). The counts free the slot; until parking, a waiting terminal stays alive, so live terminals can exceed `max_concurrent_workers` by the number of waiting runs. |
| Q6 | Which goal clause does Issue 248 serve? | G7 (it runs its own development), G1 (one contact). | G7: M7 needs it. |
| Q7 | One range at a time, or several? | (a) One active range per project. (b) Several. | (a). Two ranges sharing slots need an order between them; nothing on 2026-11-21 asked for it. |
| Q8 | Should a dispatch that names no range need the person's key while a range is active? | (a) No: as today, the secretary dispatches on the person's word in the conversation; such runs count against `max_concurrent_workers` and show with no range. (b) Yes: outside a range, `dispatch` needs the person key or a range. | (a) for the first version: (b) changes every dispatch today, and the conversation is where the person asks for one-off work. Revisit with the screen, where a person's dispatch is one button. |
