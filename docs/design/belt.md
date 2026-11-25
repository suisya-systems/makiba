# The belt: work inside an approved range

Status: design note for Issue 248, 2026-11-21; the owner's decisions of 2026-11-22 are taken in (section 9). Steps 1 to 4 are built (Issues 257, 282, 283 and 284): section 11 says how, and where the build settled a finding of the design review or departed from this note; the rest is not built. Read beside [plan.md](plan.md) (R4, M7), [spec.md](spec.md) (sections 5, 10, 12 and the read surface) and the skill `skills/conveyor/SKILL.md` (Issue 188), which this note turns from a skill into records. A design review (one round, before the commit) was folded in; what it found is marked "review" where it changed the design.

Today the person is the clock: a run closes, `makibad` scans, the secretary proposes, the person picks. The conveyor skill lets the secretary keep going inside a scope, but the scope lives in the conversation, the count of slots is the secretary's own, and every stop holds a slot. On 2026-11-21 claude-org-ja ran this belt by hand over about a dozen makiba Issues; this note says what of that becomes a record or a verb here.

## 1. What the person does

| Step | What the person does | What they see |
| --- | --- | --- |
| Design it with the secretary | In the conversation, as often as they like: the secretary drafts a range (which Issues, as a list, a condition or both; how many at once; how many tasks and hours; the text every task starts from), the person changes it, the secretary records the new draft. Each draft is a `proposed` row (`range propose`), or replaces the previous one (`range propose --replaces <id>`). Nothing is dispatched. | Each draft as `makibad` stored it, with what it would admit now (next row). |
| Approve | Once the draft is right, approve it with their own key: `makiba range approve <id>` in their shell (later a button on the screen of Issue 169). Only this step needs the key. | Before confirming, from a read `makibad` made for that draft (3.3): the Issues it admits now, in the order they would go in; each Issue the list names or the condition matches that is left out, with why (an open dependency, an open pull request, a run of its own, a `skip`); the budget and `at_once`; `brief_head`; the merge approval mode. The approval records which read it showed (`approved_read_at`). |
| Watch | Nothing. Tasks go in as slots free up, without a call. | The whole range as one list in a fixed order: every item with its place (not yet in and why, in work, waiting for the person, in review or waiting to land, landed, skipped), the count per place and the budget used (7.1). |
| Answer | When something on the belt needs them, they get one question in the conversation, with a recommendation. While it waits, the other tasks go on and the free slot is filled. | Each stopped task with its line, its reason and how long it has waited. |
| Pause | Say pause (or press it). Nothing new goes in; work already running goes on. Resume with their key. | The range as paused. |
| End | Say end, or let it run out (budget spent, nothing left inside). Work already running finishes; nothing is abandoned. | The range as ended, with why. |

## 2. The range as a record

### 2.1 Fields

| Field | Meaning | Who evaluates it |
| --- | --- | --- |
| `id` | Integer, from `makibad`. | - |
| `project` | One registered project (its slug). One active range per project in the first version: a check on every move to `active` (`approve`, `resume`), not a property of the records (section 10). | `makibad` |
| `issues` | An explicit list of Issue numbers, or empty. | `makibad` |
| `takes` | A condition over open Issues, or empty. A fixed grammar, joined by AND: `label:<l>`, `no-label:<l>`, `milestone:<m>`, `clause:<=G<n>` (the goal clause the Issue names, `docs/goals.md`). A candidate is in the range when it is in `issues` or matches `takes`. | `makibad`, on its own read of the project's open Issues (section 3.3). No model. |
| always | Every candidate must also be open; have no run of its own not `completed` or `abandoned` (by `runs.issue_refs`, which a range dispatch fills), and no run of this range at all (an abandoned one included: a retry is the secretary's dispatch naming the range); have no open pull request that closes it; and have every Issue named on a `Depends on` line of its body closed. A dependency whose state cannot be read counts as open. A line of its body that starts with `Blocked by`, `Depends on` or `Requires` and names no `#<n>` (a dependency in words) leaves it out as `dependency_unreadable` (Issue 299). | `makibad` |
| `at_once` | Runs of the range that count at a time (section 4). Never more than `max_concurrent_workers`. | `makibad` |
| `budget_tasks` | Tasks the range may dispatch in all, follow-ups included. Taken at dispatch, never given back. Required. | `makibad` |
| `budget_hours` | Wall-clock hours from approval. Optional. | `makibad` |
| `budget_fixes` | Fixes of a task's pull request: heads recorded with `pr pushed` after the first, except a head whose only new commit is a clean merge of the base (Issue 358). Default 3. A stop of that task, not a guard on the push. | `makibad` |
| `follow_ups` | `inside` as the default of a proposal (Q3, step 4); a range approved before step 4 stays `ask`. `inside`: an Issue created after approval in the same repository, whose body has a line `Follow-up of #<n>` naming an Issue of the range, is in the range (and takes from `budget_tasks`), unless a line `Needs host:` names a host the project does not list or a line `Stop line:` names the stop line it would hit (then it is asked for). `ask`: it is not, even when it matches `takes`, until a new range is approved. | `makibad` |
| `design_ahead` | `yes` or `no`. `yes`: a candidate whose only open dependencies are tasks of this range with a pull request may be dispatched for its design only (section 4.3). | `makibad` |
| `brief_head` | The text every brief of the range starts with (conventions, verification depth, what to stop on), written in the proposal and approved with it. | - |
| `approval_mode` | `merge_approval_mode` as `makibad` read it at approval, the way `gate` reads it. | `makibad` |
| `proposed_by`, `proposed_at`, `replaces` | The proposer (the secretary or the person); the draft this one replaces. | `makibad` |
| `approved_by`, `approved_at`, `approved_read_at` | The person whose key approved (a field: `person` while makiba knows one person), the time, and the read shown at approval (its content is kept in the `range_approved` event, since later reads replace `range_candidates`). The range's stops are put to `approved_by`. | `makibad` |
| `status` | `proposed`, `active`, `paused`, `ended`. | `makibad` |
| `ended_reason` | `person`, `secretary`, `budget_tasks`, `budget_hours`, `used_up`, `scan_error`, `replaced` (also a draft replaced by a newer draft). | `makibad` |

The condition and `brief_head` must be enough to admit a task with nobody looking (review): `makibad` dispatches in the same step a slot frees, so a candidate that needs a person's or the secretary's look first belongs in an explicit list, not in `takes`. The secretary may take a candidate out before it is dispatched (`skip`, with a reason, recorded); it can never put one in. What could only be judged by reading (on 2026-11-21: "the body asks for no decision of the person first") is the secretary's `skip`, or the proposal's list.

### 2.2 Where it is stored

| Store | Holds |
| --- | --- |
| `state.db`, table `ranges` (migration v8) | One row per range: the fields above. |
| `state.db`, table `range_candidates` (migration v8) | The newest read for a range (keyed by `range_id`, a draft's included): every Issue the list names or the condition matches, with title, body, comments, labels, `Depends on` numbers, closing pull request, read time, and `admitted` or the reason it is left out. What the approval shows and a dispatch's brief is made from. |
| `state.db`, `runs.range_id` (migration v8), `runs.issue_refs` (exists) | The range a run was dispatched under (null outside every range), and its Issue. |
| `state.db`, `events` | `range_proposed`, `range_approved`, `range_paused`, `range_resumed`, `range_ended`, `range_read`, `belt_dispatched`, `belt_skipped`, `belt_stopped`, `run_parked`, `run_unparked` (section 7). |
| `state.db`, `pending.context_json` | On an entry that is a stop of a range task: `stop_line`, `range_id`, `to` (the range's `approved_by`), `kind` (5.2); `recommend` (exists) is `yes` or `no` on such an entry, so that whether the person took it is computed from `answer`. |

Why not a file (claude-org-ja keeps `.state/conveyor/scope-contract.md`):

- A file the secretary can write is a range the secretary can widen. `state.db` is written by `makibad` only (spec section 1), and no agent role can write it.
- The counts (tasks taken, slots taken) must move in the same transaction as the dispatch, the escalation and the close that change them.
- A renewed secretary reads the range from `status`; the handover no longer has to carry it whole (the skill's section 6 today).
- The screen of Issue 169 reads `makiba status --json` and `makiba events --json`, never a file.

### 2.3 Who may write it

| Verb (`range ...`, new) | Secretary | Person | Effect |
| --- | --- | --- | --- |
| `propose [--replaces <id>]` | yes | yes | A `proposed` row and a read for it (3.3); `--replaces` ends the earlier draft (`replaced`). Changes nothing else, so the secretary drafts freely. |
| `approve <id>` | no | yes (person key) | Prints the draft and its newest read (section 1), asks for confirmation, then `proposed` to `active`; `approval_mode`, `approved_by`, `approved_at`, `approved_read_at` written; a fresh read and the first dispatches follow (section 3). |
| `pause <id>` | yes | yes | No new dispatch. Narrowing, so the secretary may. |
| `resume <id>` | no | yes | `paused` to `active`. |
| `end <id>` | yes | yes | `ended`, reason `secretary` or `person`. |
| `skip <id> <issue> --reason` | yes | yes | Takes one candidate out of the range before it is dispatched (`belt_skipped`). |

There is no verb that changes an active range. Widening anything (more Issues, budget, `at_once`, `follow_ups` from `ask` to `inside`) is a new proposal, approved again; approving it ends the old one (`replaced`) in the same transaction. The old range's runs keep their range (where a run came from, and each range's budget, stay readable); the runs of the ranges a range replaced count against its `at_once` (as built in step 1, finding 3).

What the range does not bound (review): a `dispatch` that names no range. That is today's path, the secretary acting on the person's word in the conversation, and it stays as trusted as today (Q8): a one-off task needs no range and no key. It is not silent: such a run counts against `max_concurrent_workers`, so it takes from the belt's room, and the read surface shows it as a run with no range.

### 2.4 How it ends, and how it pauses

| Transition | Cause | By |
| --- | --- | --- |
| `active` to `ended` | `budget_tasks` all taken, or `budget_hours` past, and every task of the range closed; `used_up` (a complete read finds no candidate, none left out only for an open dependency, an unreadable Issue or a dependency in words, and no task of the range is open); `scan_error` (the read reports the project unset or in error, never read as "no candidates"). | `makibad` |
| `active` or `paused` to `ended` | `end`, or a replacing approval. | Secretary or person |
| `active` to `paused` | `pause`; two launches of the range failed in a row (a verified launch resets the count); two refusals by the gate of different pull requests in a row (a merged one resets it). | Secretary, person, `makibad` |
| `paused` to `active` | `resume`. | Person |

Once `budget_tasks` are all taken or `budget_hours` is past, nothing new is dispatched; the range stays `active` and is read as ending (each candidate `not_in` is `budget`, none `no_slot`) until its last task closes, and only then is it `ended` with that reason. "Ending" is that stretch, never a status of its own. In every case runs already dispatched go on to their own end: the secretary still pushes, merges per the mode and closes them. Nothing is abandoned by an ending.

## 3. The loop

### 3.1 Who holds it: `makibad`

The rule the owner cares about: whoever holds the loop is outside the actor it restrains. The range restrains the secretary (what may go in without asking, how much). So the secretary must not be the one who decides that a candidate is inside, that a slot is free or that the budget allows one more. `makibad` holds the clock, the count and the dispatch; the secretary keeps the judgment (answering workers, the pull request, the merge under the mode, the close, the stop lines that are judgment).

`makibad` dispatches the next task itself (decided, Q2): a run with the range's `brief_head`, then the Issue's title, body and comments from `range_candidates`. That also ends copying Issue text to workers by hand; a dispatch that names `issue` gets the Issue's text the same way, read at dispatch (Issue 317, spec section 12). The task id is `r<range id>-<issue>`, so a task is dispatched once whatever restarts. The secretary may still dispatch a range candidate with a brief of its own, naming the range; `makibad` refuses it when the Issue is outside the range or no slot or budget is left.

### 3.2 The count

A slot is free when both of these are below their limit:

| Count | Counts | Does not count |
| --- | --- | --- |
| The range's, against `at_once` | Its runs `queued` (from the moment of dispatch, so one transaction cannot admit more than `at_once`; review) or `in_use`. | Runs waiting only for the person (4.1), `review`, `parked`, closed. |
| `makibad`'s own (`dispatch::busy`), against `max_concurrent_workers` | As today: runs `in_use`, and `queued` or `suspended` with a launch under way. | Changed (review): runs waiting only for the person, so that a stop frees a launch with one slot in all. |

A run that comes back (answered, unparked, sent back to rework) counts again at once, even above the limit; nothing new goes in until the count is below it.

### 3.3 Reading the candidates

The triage pass cannot serve the clock as it is (review): it scans only after a close, at most every 30 minutes, keeps the top 3 before any range is applied, and keeps no Issue text. So the range has its own read: on `propose` (for the draft, so that the approval shows what it would admit), on approval, on `resume`, after each close of a range task, and every 10 minutes while the range is active (a first proposal), `makibad` reads the project's open Issues whole (body, comments, labels, milestone) and the open pull requests with the Issues they close, outside any transaction. It applies the range to every Issue before any cut, looks up the state of each dependency not in the read, and writes `range_candidates` and a `range_read` event in one transaction. A read that fails keeps the previous one and is said in the log; only a complete read can end a range `used_up`.

### 3.4 State table

"Next candidate" is the first row of `range_candidates` in the read's order (the goal clause the Issue names, none last, then the Issue's number: step 1 does not carry the triage scan's other keys) that the records still allow (no run of its own, a slot, budget) and that does not depend on a task waiting for the person (4.2).

| Event | `makibad` does | The secretary does |
| --- | --- | --- |
| The person approves a range | Reads, then dispatches next candidates until no slot is free (each `belt_dispatched`). | Tells the person what went in. |
| A slot is free (any cause below) | In the transaction of the event that freed it: dispatches the next candidate from the newest read. With none: ends it `used_up` when 2.4 allows. | Nothing. |
| A worker reports `done` | Run to `review` (as today); it stops counting. Slot free. | Push, pull request, `pr` (skill `pull-request`). |
| CI failed | Nothing new. | Sends the worker back. |
| The worker starts the rework | `report start` from a worker at `review` moves its run to `in_use` (new, review): it counts again. | Nothing. |
| A new head is recorded (`pr pushed`) | At the `budget_fixes`-th fix after the first head: a stop of the task, line budget (section 5). A clean merge of the base is no fix. | - |
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

A candidate depends on a task waiting for the person when a `Depends on` line of its body names that task's Issue. `makibad` passes over such a candidate while the task waits and takes the next. Only the body's `Depends on` lines are read as dependencies (not comments, not other words; review: the triage parser reads more, and the range uses this one grammar). A dependency the Issue does not write is the secretary's `skip`. A dependency written in words is not read as none: a line of the body that starts with `Blocked by`, `Depends on` or `Requires` (after an optional quote, heading, list marker, task box or emphasis) and holds no `#<n>` leaves the candidate out as `dependency_unreadable (<the line>)` until the line names its Issue; which Issue the words mean is not guessed (Issue 299).

### 4.3 A task waiting for another may still do its design

This is for a dependency that waits for its landing, not for an answer (4.2 is the other case). With `design_ahead: yes`, a candidate whose open dependencies are all range tasks with a pull request is dispatched with one line added to its brief: design only, stop and report before any code, because `#<n>` (each listed) has not landed. Its run records the set it waits for. When it reports, it is parked: it waits for a landing, not a person, and does not count. When every Issue of the set has landed, `makibad` unparks it with one message naming each landed Issue and main's head: update the branch and build. An Issue lands through the task it waited for or through any other run of the project that names it (a one-off dispatched with `issue`, a redo; Issue 488). A dependency closed `abandoned` is a stop of the waiting task for the person (line 1). How step 4 built it is in 11.3.

## 5. The stop lines in makiba's terms

Each stop of one task is a pending entry for the person with `stop_line` set, and a `belt_stopped` event; the belt goes on. `makibad` writes both when a record decides the line; the secretary writes them (`pending add` with the line, then `escalate`) when it is judgment.

| Line | Detected by | Held today? |
| --- | --- | --- |
| 1. A decision that is the person's | The secretary's `escalate` (judgment; the record makes it visible and frees the slot). Skill `pull-request` line 3 for a pull request. | Judgment only. |
| 2. A change outside the range | A candidate outside the range: never dispatched by `makibad`, and refused when the secretary names the range (code). A dispatch naming no range: not bounded (2.3, Q8 decided so). A change beyond its brief inside a task: judgment (skill line 3). | Not yet: no range record. |
| 3. An irreversible action | Skill line 4 (judgment). Code holds parts: `push` is never forced, `down` and the base are the person's, and a change with no declared test merges under `acceptance` only when it is Markdown or under `docs/`. Not held: a release tag, a change of settings or access made through `gh`, a change to `Cargo.toml` or the lock file (which the gate does not compare with the test commit unless declared), and a script changed on disk outside the repository. | Parts, by `push` and `gate`. |
| 4. The budget | `makibad` counts `budget_tasks`, `budget_hours` (whole range) and `budget_fixes` (one task) from records. | No. |
| 5. A suspicion that a test was rewritten to pass | `gate`: the test commit is the first and unchanged, the declared paths and test setup at head are as at the test commit, the manifest digest is pinned. Not held: an edit to an existing test outside the declared paths (on 2026-11-21 every such pull request was landed by the owner by hand). Proposed for step 2: `gate` reports a change to an existing file under `tests/` not declared, and the belt makes it a stop. | Yes for declared tests. |
| 6. A refusal by the gate | Every `gate` answer with `allow` false (review: `rejected`, and `no_manifest` refused under `acceptance`), including the refusals decided before the git checks (a head or CI mismatch), which then also write an `acceptance_gate` row so that a record exists. One stop per pull request and head. | The refusal, yes; the stop record, no. |
| (mode) A range approved under `human` | Judgment, as today: the secretary merges without asking only when the range's `approval_mode` and the mode read now are both `acceptance` (skill `conveyor`). Review: substituting `human` in the gate would not hold it, since the gate under `human` refuses less, and no record of the person's merge word exists for the gate to read. | Skill only. |

Today's acceptance gate (Issue 47) already holds: the head and passed CI must match, the declared test failed before and passes at head, the test and its setup are unchanged since the test commit, a pull request without a declared test merges under `acceptance` only for docs, and the merge-gate hook refuses every other way to merge from a command line.

### 5.1 The design step and the reviews

On claude-org-ja every task of 2026-11-21 wrote its design first, had it read once by a second model before any code, and had its diff read at the end. It paid: the design reviews found blocking mistakes before code in Issue 140 (a code worker could write the clone's shared `.git`; the network did not hold the push line), Issue 241 (a link planted under the run's git folder; a submodule's configuration run inside `makibad`), and Issues 231, 136, 236 and this note. The diff reviews found less each round.

| Piece | Where, when `makibad` dispatches |
| --- | --- |
| Design first, its review, code, the diff review | `brief_head`: steps every task of the range runs, approved with the range. `makibad` adds nothing per task. |
| A task that must stop after its design for a decision | Line 1, raised by the worker: `brief_head` names which decisions are the person's (by `kind`, 5.2) and says "ask before code"; the worker asks, the secretary escalates, the run stops counting and the slot is filled (4.1). No field of its own. |
| Whether a review by another model runs, with which program, how many rounds | Issue 135, undecided. Today makiba's worker rules do not ask for one. |

Issue 135 is a named risk, not a dependency of step 1. Step 1 is the clock and the count, which work the same with or without reviews; when 135 is decided its rule goes into `brief_head` (and its round cap into code) with no change to the range. Until then the reviews are not run, and what is lost is the class above: design mistakes on a trust boundary found before code. They reach the code and are found by CI, by the person in the pull request, or not at all. Meanwhile a proposal whose Issues touch a trust boundary (git, the sandbox, keys, the push line) puts "stop after the design and ask" in its `brief_head`, so that the person reads the designs a review would have read.

### 5.2 Answering with the recommendation: decided by stops, not days

The first version has no `answers: recommend` (Q4). It is decided after the first ranges have stopped for the person 10 times on line 1 (N = 10, proposed), not after a number of days, from this record:

| What | Where |
| --- | --- |
| Each stop: the question, its `kind` (`design`, `scope`, `order`, `naming`, `other`), the recommendation (`yes` or `no`, with `why`) | `pending.question` and `context_json`, written by the secretary's `pending add` for the stop. |
| What the person answered, and so whether it was the recommendation | `pending.answer`, written by `answer`. |
| Read | `makiba status --json`, `ranges[].stops[]` (every line-1 entry of the range, any status) and `ranges[].stops_taken` (per `kind`: stops, answered as recommended). The screen of Issue 169 shows the same. |

`answers: recommend` is then a small addition to the range: a list of `kind`s the secretary may answer with the recommendation, each such answer recorded as by the secretary and listed for the person. It never covers lines 2 to 6: a change outside the range, an irreversible action, the budget, a suspicion that a test was rewritten, a refusal by the gate.

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
| Answering workers' questions "all recommended" on the owner's standing word | Stays with the person in the first version (Q4); each stop records the recommendation and the answer, which decides `answers: recommend` (5.2). |
| The order of landings to avoid repeated merges of main | Stays with the secretary. |
| Landing on the owner's screen (CI or script changes, edited tests, no declared test) | Stays with the person: stop lines 3 and 5. |
| A design review and a diff review by a second model in every task | Not carried until Issue 135 decides it (5.1); `brief_head` is where it goes. |
| Setting the review round cap per task | Not carried yet (L-K10, Issue 135); `budget_fixes` counts heads meanwhile. |
| Host runs and probes | Stays with the person or the secretary outside the belt; a task that needs one is a stop (line 1). |
| Re-running a flaky CI job | Stays with the secretary. |
| A task list (`TaskList`) as the belt's display | Record: the read fields of section 7. |

## 7. Read fields for the screen of Issue 169

Additions to the table `Status field` (spec section 12). The `ranges` rows ship with step 1, since a renewed secretary reads the range there (review), and with them the item list of 7.1 (its `parked` stage from step 3); the `belt` rows with step 5.

| Status field | Source |
| --- | --- |
| `ranges[].id`, `.project`, `.status`, `.ended_reason` | `ranges`: every range not `ended`, every ended range with a run not closed, and the newest ended range of each project |
| `ranges[].issues`, `.takes`, `.brief_head` | `ranges`, as approved |
| `ranges[].at_once`, `.follow_ups`, `.design_ahead`, `.approval_mode` | `ranges` |
| `ranges[].budget.tasks`, `.hours`, `.fixes` | `ranges.budget_*` |
| `ranges[].used.tasks`, `.hours` | runs with the range's `range_id`; hours since `approved_at` |
| `ranges[].skipped[]` | `belt_skipped` of the range: `issue`, `reason` |
| `ranges[].read_at` | the newest `range_read` |
| `ranges[].approved_by`, `.approved_at`, `.approved_read_at` | `ranges` |
| `ranges[].candidates[]` | `range_candidates` of the range: `issue`, `title`, `admitted`, `left_out_why` (also for a draft) |
| `ranges[].items[]`, `.places` | 7.1 |
| `ranges[].stops[]`, `.stops_taken` | `pending` entries with the range's `range_id` and line 1: `question`, `kind`, `recommend`, `answer`; counts per `kind` (5.2) |
| `belt[].range_id`, `.task_id`, `.issue` | runs with a `range_id`, not `completed` or `abandoned` |
| `belt[].place` | `launching`, `at_work`, `waiting_person`, `parked`, `review`, `stopped`, from `runs.status`, the newest launch's stage and the run's open entry |
| `belt[].counts` | whether it counts against `at_once` (3.2) |
| `belt[].stop_line`, `.stop_reason`, `.pending_id` | the newest `belt_stopped` of the run while its entry is open; the reason's first line |
| `belt[].waits_for` | the Issues a design-ahead run waits for |
| `belt[].since` | when its place began |
| `pending[].stop_line`, `.to` | `pending.context_json` |
| `runs[].range_id` | `runs.range_id` |

The table `Event field` does not change; the new kinds go into spec section 10 with their payloads:

| Kind | Payload |
| --- | --- |
| `range_proposed`, `range_paused`, `range_resumed` | `range_id`, `reason` (for a pause) |
| `range_approved` | `range_id`, `approved_by`, `approval_mode`, `shown`: the read printed at approval (each Issue in order, admitted or with its left-out reason) |
| `range_ended` | `range_id`, `reason` |
| `range_read` | `range_id`, `candidate_refs`, `complete` |
| `belt_dispatched` | `range_id`, `issue`, `task_id`, `design_only` |
| `belt_skipped` | `range_id`, `issue`, `reason` |
| `belt_stopped` | `range_id`, `task_id`, `line`, `reason`, `pending_id` |
| `run_parked`, `run_unparked` | `task_id`, `reason` |

### 7.1 The whole range as a list

A screen shows every item of the range, not only the tasks on the belt (the owner, 2026-11-22: seeing it puts people at ease). `ranges[].items[]` has one entry per Issue the range has admitted, skipped or would admit now: `issue`, `title`, `place`, `why` or `stage`, `since`, `task_id` when it has a run. `ranges[].places` counts the items per place, so a screen shows the range's results (`landed`) beside its remaining work. A run with an open stop is `waiting_person` whatever its status; otherwise the rows below decide, top first.

| `place` | Read from | Detail |
| --- | --- | --- |
| `not_in` | `range_candidates` of the newest read, with no run of the range and not skipped | `why`: `no_slot` or `budget` when admitted; otherwise its left-out reason, such as `waits_for #<n>` (an open dependency, or a task waiting for the person, 4.2) or an open pull request. |
| `in_work` | runs with the range's `range_id`, `queued`, `in_use`, `suspended` or `failed` | `stage`: `launching`, `at_work`, `design_only` (4.3), `launch_failed`. |
| `waiting_person` | the run's open entry with `stop_line` (`belt_stopped`) | `why`: the line and the reason's first line; `since`: the stop. |
| `review` | runs at `review` or `parked` with no open stop, with their pull request watch | `stage`: `no_pr`, `ci`, `to_land` (CI passed, merge under the mode), `waits_landing #<n>` (a parked design-ahead run, 4.3). `since`: when it went into review (the done report that moved it, or its newest design report). |
| `landed` | runs `completed` with the range's `range_id` and a `pr_merged` event: their pull request recorded with `pr`, read merged by `makibad` | `pr`, `pr_title`, `since` (the close): what landed, not a tick only. |
| `closed` | runs `completed` with no merge recorded (no pull request recorded, or not read merged yet) | `why`: `no merge recorded`. A close says the secretary closed it, not that it landed (finding 5). |
| `skipped` | `belt_skipped` events of the range; runs `abandoned` | `why`: the recorded reason. |

Order: by admission, so that the list does not jump between reads. Every item with a run is placed by the id of its `belt_dispatched` event, and a skipped one by its `belt_skipped` event; these never move. Items `not_in` follow, in the order of the newest read; only this tail may reorder when a read changes the triage order, and an item leaves it for its admission place when dispatched.

How long: runs and events are kept, so a landed or skipped item stays readable as long as its range is listed in `ranges[]`: while the range is not ended, while it has a run not closed, and after that for as long as it is the newest ended range of its project. Older ranges remain in `makiba events --json`.

Beside it, the triage scan's `next_work` (Issue 256) is another list: what the secretary would propose next outside any range. The range's list is what the range will admit. An Issue may be in both; `ranges[].items[]` names the range, `next_work` does not. While a range is active, a screen shows the range's list first and `next_work` under it as "outside the range".

## 8. Not in the first version, and the order of building

Not in the first version: parking terminals (step 3, Q5), `design_ahead`, `follow_ups: inside` (every follow-up is asked until step 4, Q3), `budget_fixes` and `budget_hours`, the stops `makibad` writes itself, the gate's check of edited tests, `answers: recommend` (5.2), several active ranges (section 10), the `belt` read fields, the screen. A one-off dispatch never needs a range (Q8).

| Step | What | Declared test |
| --- | --- | --- |
| 1 | The range record (`ranges`, `range_candidates`, `runs.range_id`, `range propose [--replaces]/approve/pause/resume/end/skip`, `at_once`, `budget_tasks`), its read (3.3) for drafts and active ranges, the approval showing that read, `makibad` dispatching the next in-range candidate itself when a slot frees, an escalated entry counting in neither count, the stop record of 5.2, and the `ranges` read fields with the item list of 7.1. Everything keyed by range id (section 10). This alone removes the person as the clock. | `tests/belt.rs`: with a range approved for two tasks and one slot (`max_concurrent_workers` 1), the second is dispatched when the first closes with no call by the person; a task outside the range is not dispatched; an escalated stop leaves a pending entry and the next task is dispatched. |
| 2 | Stops written by code: every gate refusal and the budgets as `belt_stopped` with a pending entry; `budget_hours`, `budget_fixes`; rework counting again; launch failures and gate refusals pausing the range; the gate reporting edited tests. | `tests/belt_stops.rs`: a refused gate verdict on a range task leaves a pending entry with `stop_line` `gate` and the next task is dispatched. |
| 3 | Parking and unparking terminals; `parked` status; `up` leaves parked runs alone. | `tests/belt_park.rs`: a run waiting on an escalated question has no terminal after the wait, and `answer` brings it back on its conversation. |
| 4 | `design_ahead`, and `follow_ups: inside` as the default of a proposal (built, 11.3). | `tests/belt_ahead.rs`: a dependent candidate is dispatched design-only and unparked when its dependency closes completed. |
| 5 | The `belt` read fields, then the screen (Issue 169). | `tests/read_surface.rs` grows the rows above. |
| later | `answers: recommend` (after N stops, 5.2); several active ranges (section 10); the review rule once Issue 135 is decided (5.1). | Each with its own. |
| - | The skill `conveyor` shrinks to the judgment that stays with the secretary, in the pull request of each step. | `tests/conveyor_skill.rs`. |

## 9. Decisions of the owner (2026-11-22)

| # | Question | Decided |
| --- | --- | --- |
| Q1 | How the person approves a range | Only with the person key. The range is designed with the secretary in the conversation; only the approval needs the key (section 1, 2.3). |
| Q2 | Who dispatches the next task | `makibad`, with the approved `brief_head` and the Issue text (3.1). The design step and the reviews go in `brief_head` (5.1). |
| Q3 | Follow-ups found on the way | Asked each time for now; `inside` becomes the default of a proposal from step 4. |
| Q4 | May a range carry "answer with the recommended option" | No for now; decided after N = 10 stops from their record, not after days (5.2). |
| Q5 | Parking terminals | Step 3, after the counts. |
| Q6 | Goal clause of Issue 248 | G9, "it runs its own development". |
| Q7 | One range or several | One active range per project now, as a check; the records are built for several ranges and several people (section 10). |
| Q8 | A dispatch naming no range while a range is active | Needs no person key, as today (2.3). |

## 10. Several ranges, several people

The first version refuses a second active range of a project in `approve`, and nothing else assumes one. What several ranges (and several people) would need, part by part:

| Part | In the first version | What several need |
| --- | --- | --- |
| `ranges` (2.1) | `approved_by` is a field, `person` while makiba knows one person. | Who owns a range: `approved_by`, the person whose key approved; only that person resumes it and approves its replacement. A person key per person (today there is one). |
| `range_candidates`, events, `runs.range_id` (2.2) | Keyed by `range_id`. | Nothing more. |
| The counts (3.2) | `at_once` counted per `range_id`; `max_concurrent_workers` shared with runs outside ranges. | An order between ranges that share the slots of `max_concurrent_workers`: a free slot goes to the active range with the fewest runs counting, ties to the earlier approval. A field `share` if equal turns are not enough. |
| The candidate read (3.3) | One read per range, each keyed by its id. | A candidate that matches two ranges: it goes to the range that dispatches it first; the always-rule (no run of its own, by `runs.issue_refs`) keeps the other from dispatching it again. The read surface lists it in both, with `left_out_why: in range <id>` in the other. |
| The state table (3.4) | Each event names a run, hence its `range_id`; "a slot is free" runs the order above. | The task id `r<range id>-<issue>` already tells the ranges apart. |
| The stops (section 5) | A stop is put to `pending.context_json.to`, the range's `approved_by`. | The secretary puts the question to that person; the screen shows each person their own stops. |

What must be true in step 1, so that several ranges are an extension and not a rewrite: every count, read, event and status field is keyed by the range id (no "the active range" in code or in the read surface; `ranges[]` is a list); `approved_by` and the person a stop is addressed to are fields, never the constant `person`; and the one-range rule lives in the move to `active` (`approve`, `resume`) only, not in a unique index.

## 11. Step 1 as built (Issue 257)

Built: the records (migration v8: `ranges`, `range_candidates`, `runs.range_id`), the verb `range` (`propose`, `approve`, `pause`, `resume`, `end`, `skip`), the read (spec section 5.8), the dispatch by `makibad` in the transaction of the done report, the close, the escalation or the read that freed or found a slot, the counts, the stop record and `ranges[]` on the read surface (spec section 12, table `Range field`). Not built: `budget_hours`, `budget_fixes`, `follow_ups: inside` (a follow-up is left out as `follow_up (ask)`), `design_ahead`, parking, the stops code writes itself, `dispatch` naming a range (a range's tasks are dispatched by `makibad`; a one-off dispatch names its Issue with `--issue`), and the `belt[]` and `pending[].stop_line` read fields.

Departures from this note, decided with the owner's standing answer of 2026-11-22: runs are not moved on a replacement (2.3); the place `closed` (7.1); the read order is the goal clause then the number (3.4).

The nine findings of the design review (Issue 248, 2026-11-22):

| # | Finding | In step 1 |
| --- | --- | --- |
| 1 | Rework and suspended runs in the count | Built. `report start` from `review` puts a run back `in_use`; the range's count takes `queued`, `in_use`, `suspended` and `failed` (until closed, so a broken launch cannot spend the budget before the automatic pause of step 2), less runs waiting only for the person. Step 2 built that pause, and `failed` left the count (11.1). |
| 2 | A one-off dispatch and `runs.issue_refs` | Built. `dispatch --issue` fills `issue_refs`; a range leaves out and never dispatches an Issue an open run names. A one-off without `--issue` stays unseen (the delegate skill asks for it). |
| 3 | Replacing a range, its budget | Built: runs keep their range; `at_once` counts the replaced chain; the budget counts the range's own dispatches. |
| 4 | A failed read | Built. A failed or truncated read is recorded `complete` false and keeps the candidates, which are not dispatched from: a range dispatches only from a newest read that is complete and was started after its newest approval or resume. Only a complete read ends a range `used_up`. |
| 5 | What proves a landing | Built. `pr_merged`, from the run's recorded pull request read merged; `landed` needs it, `closed` otherwise. |
| 6 | A parked run and another open stop | Deferred to step 3, which parks. In step 1 nothing parks; a run with any escalated entry counts in neither count until every one is answered. Decided in step 3 (11.2): parking is computed from the records on each pass, never a latch. |
| 7 | What the gate holds | Section 5 now says what it does not hold (`Cargo.toml`, the lock file, a script on disk); the gate's check of edited tests is step 2 (built, 11.1). |
| 8 | The resume brief | Built. A renewed secretary's brief names the ranges in force first, right after its fixed line, and says to read `status` before anything else. |
| 9 | `budget_hours`, "ended" and "ending" | Worded in 2.4: "ending" is the stretch before the last task closes, never a status. `budget_hours` is not built in step 1. |

### 11.1 Step 2 as built (Issue 282)

Built: the stops `makibad` writes itself (spec section 5.8), the budgets `budget_hours` and `budget_fixes` (migration v9, which makes `ranges` again to add `budget_hours` to the reasons a range ends), the two pauses of 2.4, the gate's report of edited tests, and the three sentences step 1 left in the rules and skills.

| Part | As built |
| --- | --- |
| A stop written by code | One function writes all of them: a pending entry made `escalated` at once (`context_json`: `stop_line`, `range_id`, `to`, `by` `makibad`, the five context parts, `recommend_answer`), `belt_stopped` by `makibad`, a notice to the secretary, and the fill, in the transaction of what caused it. One per subject: a pull request and head, a task's count of fixes, a budget of a range. A stop of a whole range has no run. |
| Line 6, the gate | Every answer of `gate` writes an `acceptance_gate` row, the refusals decided before the git checks included. `allow` false on a range task's pull request is a stop of line `gate`, recommended answer `no`. |
| Line 5, edited tests | `gate` reports the files under a folder `tests` that were there before the test commit, are not declared, differ at head and did not come from the base. For a range task it is a stop of a line of its own, `tests`, recommended answer `none`. |
| Line 4, the budgets | `budget_fixes`: the fix recorded with `pr pushed` that reaches it (and each `budget_fixes` fixes after) is a stop of the task; the push is never refused. A head whose only new commit is a clean merge of the base (two parents, the head recorded before and a commit of the base, with the tree git's own merge of them writes) is recorded as any push but is no fix (Issue 358). `budget_tasks` and `budget_hours`: a stop of the range when the budget is reached while a candidate the dispatch would take is left; a range that takes all it holds ends without a question. `budget_hours` past: nothing new goes in, and the range ends `budget_hours` when its last task closes. |
| The pauses (2.4) | Two launches failed in a row, or two pull requests refused in a row with none read merged between, pause the range; a run whose launch failed no longer holds a slot (the guard of step 1, finding 1). The range paused is the one not ended that the run's range is or was replaced by. The refusal that pauses dispatches nothing: the stop, then the pause, then the fill. |
| Rework counted again | Built in step 1 (`report start` from `review`). Step 2 adds: a head recorded for a pull request withdraws the `gate` and `tests` stops of its older heads, so a run sent back counts again once its new head is recorded. |
| The mode (section 5, last row) | `gate` reads the range's `approval_mode` for a range task's pull request. An answer that allows carries `warn`, which the merge-gate hook turns into a question to the person, unless the range's mode and the mode read now are both `acceptance`. |
| `escalate` | Refuses a range task's entry made by `add` without the stop's kind and the recommended answer. |
| The answer | `answer` on a stop `makibad` wrote goes to the secretary, not to the worker, and a stop with no run can be answered. An answer changes no range. |
| The skill `conveyor` | Shrunk by what is now held: the pair of modes, the stop fields, the counts of fixes and failures. |

Choices of the build beyond this note, made with the secretary's answer of 2026-11-22:

- The mode pair as `warn`, not as a refusal. Section 5 found that putting `human` in place of the mode read would refuse less. So the checks stay those of the mode read now, and a pair that is not `acceptance` twice only adds the hook's question to the person; neither this nor the edited tests ever turns a refusal into an allowance.
- Edited tests under `human` are allowed with `warn` (the person is asked at the hook) and still a stop; under `acceptance` they are refused (`tests_edited`), since there nobody is asked.
- A `gate` or `tests` stop is written only for the head recorded last for the pull request. A refusal of an older head is recorded (`acceptance_gate`) and is no stop: its subject is gone, and a stop would keep the run out of the count with nothing to answer.
- A budget of the range is a stop only while it keeps a candidate out, so that a range that finishes what it planned asks nothing.
- `budget_fixes` stops again after each further `budget_fixes` fixes: a yes buys that many more.

Deferred, with their steps unchanged: parking and unparking (step 3, since built, 11.2); `design_ahead` and `follow_ups: inside` (step 4); the `belt[]` and `pending[].stop_line` read fields (step 5); `dispatch` naming a range; `answers: recommend`. Not held by the report of edited tests: a test kept outside a folder `tests` (the unit tests inside a source file), and `Cargo.toml` or the lock file (section 5, line 3). A stop of a whole range is not on `ranges[].items[]`, which lists Issues; it is in `pending` and in `belt_stopped`.

### 11.2 Step 3 as built (Issue 283)

Built: parking and waking (spec section 5.8, `src/daemon/park.rs`), the status `parked` (migration v10, which makes `runs` again), the read fields `runs[].waiting`, `runs[].unparked` and the stage `parked` of an item waiting for the person, and `down` and `up` leaving a parked run alone.

| Part | As built |
| --- | --- |
| Who is parked | A run `in_use` with an escalated question, no entry `open` and no prompt entry escalated, its newest launch verified, no message to its worker waiting for delivery, and its terminal's newest status and turn `idle` or `done` for `MAKIBA_PARK_AFTER_MS` (default 120 s), with the base read again first. Any run, in a range or not. |
| Parking | The run `parked`, its worker's keys revoked and `run_parked`, in one transaction; then its terminals ended, again on a later pass when that fails. Workplace, branch and conversation kept. |
| Waking | `parked` to `suspended` with a relaunch on the conversation of its newest verified launch, and `run_unparked` with the reason: `answer`, `message` (any other message to its worker), `answered` or `withdrawn` (it has no escalated question left), `ci_failed`, `landed`. The relaunch is told why in a line of its own (`UNPARK_PROMPT`); the answer or message comes by the delivery. |
| The count | A woken run counts again from its wake, even while an older question is still escalated, until it is parked again or escalates a newer one (section 3.2: "a run that comes back counts again at once"). |
| `down`, `up` | Leave a parked run as it is. |

Finding 6 of the design review (a parked run and another open stop) is decided: parking is recomputed from the records on each pass, never a latch. A run woken by the answer to one of two questions counts and works again; if it still waits on the other once its terminal has been idle for the wait, it is parked again.

Choices of the build beyond this note, made with the person's answer of 2026-11-23:

- An entry `open` keeps the terminal: it is the secretary's to answer, so the run does not wait only for the person.
- A relaunch that cannot end an earlier terminal of its run waits and is tried again instead of failing, for every relaunch: two workers in one workplace would be worse than a wait.
- A conflict and a review finding have no record of `makibad`'s to wake a run by; they reach a parked worker as the secretary's `send`.

Not built: parking a design-ahead run that waits for a landing (step 4, which uses the reason `landed`; built, 11.3); a notice to the person when a run is parked.

### 11.3 Step 4 as built (Issue 284)

Built: `design_ahead` and `follow_ups: inside` as the default of a proposal (spec section 5.8, `src/daemon/ahead.rs`), migration v11 (columns only added: a range approved before it stays `ask` with no design ahead), the read fields `ranges[].follow_ups`, `.design_ahead`, `.candidates[].design_only_waits`, the item stages `design_only` and `waits_landing #<n>` and `runs[].waiting` `for` `landing`, and the approval showing both and each design-only candidate.

| Part | As built |
| --- | --- |
| Who goes in for its design | A candidate held out only by dependencies, each a task of the range (or a range it replaced) with a pull request recorded, not abandoned or failed, with no pull request read closed unmerged and not waiting for the person (4.2, finding 2 of the design review). Checked by the read and again at the dispatch. Ready candidates go first. A recorded pull request of a range task triggers a read. |
| Its design | The brief's last paragraph says design only. Its `report blocked` is the design (`design_reported`); its `report done` is refused while it waits, so a waiting design cannot be closed as completed (`close --abandon` still can). |
| The count and parking | From the design report it counts in neither count, and is parked by the rule of 11.2 with the landing wait in place of the escalated question. Woken before the landing (a `send`, an answer), it counts again from the wake until it is parked again or reports again (finding 3). `park::settle` leaves it parked. |
| A landing | Every task it waits for `completed`, its pull request read merged (`pr_merged`, with the merge commit), and the merge commit seen in the project's clone after a fetch of its default branch (`landed_in_clone`). The clone is the workers' `origin`, so the worker's `git fetch origin` then brings the merge (finding 1). A failed read, a failed fetch or a merge not there yet writes nothing and is tried again every `MAKIBA_LANDING_RETRY_MS`. Then `ahead_settled` and a wake with the reason `landed`, told each landed Issue, its pull request, its merge commit and the base's head. |
| A dependency that will not land | Gone, abandoned, failed (finding 4 of the design review), or its newest pull request read closed unmerged (`pr_closed`; the landings of the tasks a design-only run waits for are read whatever their status, finding 2 of the diff review): a stop of line `decision`, once per run of it, asks whether to go on without it. `yes` lets that one task go (`ahead_released`) and the others are still waited for (finding 1 of the diff review); `no` keeps it waited for (a relaunch that lands, another pull request, or the same one reopened: read on while closed, `pr_reopened` undoes the closure; round 2). When all have landed or been let go, the run is woken, or told if not parked (round 2), with what landed and what it builds without. A merge recorded before step 4 has no merge commit: it is read again for the clone check (round 2). Only the landing of the current pull request counts, and a question about a task that landed since is withdrawn (round 3). The current pull request is the repository and number of the task's newest `pr opened` or `pr pushed`, so one registered again after another is current again, and a merge counts only with the same repository and number: a pull request replaced by one of another repository with the same number is not landed by the old one's merge (Issue 345). |
| Follow-ups | `inside` takes a follow-up of the range's Issues (and theirs) even when neither the list nor `takes` names it. One the range takes only as a follow-up is asked for (`follow_up (ask: ...)`) when a line `Needs host:` names a host the project's `network_hosts` does not list, or a line `Stop line:` names one; its own follow-ups are not taken through it (finding 3 of the diff review). `Follow-up of` names an Issue of this repository only, and the creation time is compared with the approval as a time (round 4). The skill `issue` writes these lines. |

Departures from 4.3: the design report is `report blocked`, not a state of its own; "main's head" is the head of `origin/<default branch>` in the project's clone after the fetch, which is what the worker can fetch; a dependency abandoned is not a settled wait but the same question as one that failed or was closed unmerged: go on without it, task by task.

Issue 332 (a typed send to a worker can race the park pass and be lost) is made more likely by this step: a design-only run parks right after its design report and stays parked until its dependency lands, hours rather than the minutes of a question, so a typed send from the secretary has a longer window to meet a park. The wake on a landing is a notice through the box, not a typed send, so it is not exposed. The owner's condition: this step merges after Issue 332 has landed.

Issue 488 (an Issue landed through another run): the runs that may land an Issue a design-only run waits for are the task it was dispatched to wait for and every other run of its project whose `issue_refs` names the Issue: a one-off dispatched with `issue`, a redo, a task of another range (`ahead::LANDERS`). The landing, the clone check and the PR watch's awaited set (`AWAITED`) read that one set, so the PR watch also follows such a run's pull request after its day of grace. The first of them, oldest first, that closed `completed` with its current pull request merged and the merge in the clone is the landing; the wake names it (`detail.prs[].task_id`). The question about the task waited for (abandoned, failed, closed unmerged) is still asked as before, and withdrawn when the Issue lands another way. A range candidate that depends on the Issue is admitted once the Issue reads closed, whoever closed it. Not built: not asking that question while another run naming the Issue is under way; a candidate going in for its design only on a pull request of a run outside the range (`ahead_of` still takes range tasks only).

Not built: a notice to the person when a design-only run is parked; reading the dependencies a design-only task waits for again after its dispatch (a dependency added to the Issue's body later is not waited for).
