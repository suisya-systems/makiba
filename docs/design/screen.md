# The screen: where the person puts work in, watches it and answers

Status: design note for Issue 169, 2026-11-22; redrawn twice on 2026-11-23 after the owner's readings (what each place does when it is full, 2.1; the language and light or dark, 2.2; what is always seen and what is behind a tab, 2; the centre as a judgement of the present, 2.3). Nothing is built. The drawing is [screen-draft.html](screen-draft.html): a static page with sample data, no script and no request; open it in a browser 1440 px wide or wider. It holds a quiet day with no tab open, a full day (two projects, fourteen candidates, a range of twenty, six things gone wrong, a long question) with the tab "Other runs" open, the same full day dark with the tab "Range" open, and eight panels: the other tabs open, and the other states. The owner agrees to the drawing before any code (the Issue). Read beside [belt.md](belt.md) (sections 1 and 7), [spec.md](spec.md) (section 12, the read surface) and the runbook [herdr-app.md](../runbooks/herdr-app.md). A design review (one round) was folded in: the repository in every match and request (3.1, 4), the brief head in the approval (3.1), a failed run told from a suspended one (3.3), concluded pull request watches (3.3), approving the read that was shown (4), the language (2) and the truncated scan (3.1).

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
| 2026-11-23: not everything on one screen. Always seen: the counts and whether `makibad` is alive, the questions waiting, what failed, the conversation, the range's band with its counts and the runs in work, the first few candidates. Behind tabs: what cannot go in yet, the range's brief whole, the range's every item, the runs outside the range, the pull requests, the progress of one task. A tab's heading shows its count and turns amber or red with what is behind it. | Section 2. |
| 2026-11-23: the conversation in the centre is a judgement of the present; each part is built as a region of its own so that rearranging stays cheap, and no mechanism to rearrange is built. | Section 2.3. |

## 2. The layout

One screen, but not everything on it at once (the owner, 2026-11-23): what changes little or need not always be seen is one step in, behind tabs. A top line and three columns.

**Always seen**

| Region | What it holds | Where, and why there |
| --- | --- | --- |
| Top line | The organization's name; "N waits for you"; "N went wrong"; whether `makibad` is alive and the age of the read; the two switches of 2.2. | The two counts are the only things that call for the person. |
| The range | The selected range: its terms in one line, the budget used, pause and end; a proposed range with the read to approve. Every other range of `ranges[]` as a line under it (2.1). | Left, "Put in": putting in is the most frequent act, and reading starts at the left. |
| The first candidates | The first three Issues that could go in now, with "Ask for this", and a line that counts the rest and names the tab that holds them. | Left, under the range. |
| What waits for the person | One entry large, several one at a time. | Centre, over the conversation: a question arrives in the conversation too (belt.md section 1, "Answer"), so the entry and the conversation it came from are seen together. With nothing waiting, the conversation takes the whole column. |
| The conversation | The secretary's terminal, as it is. | Centre (2.3). |
| Went wrong | Only when there is something. | Right, "Watch", at the top. |
| The range's band | The strip and the count per place (the budget is with the range, on the left). | Right, under "Went wrong". |
| In work now | The range's items that are neither finished nor waiting to go in: failed to start, waiting for the person, at work, in review; stopped ones first. | Right, the rest of the column. The person decides nothing here (the owner: its purpose is reassurance), so it sits at the side and asks for no click. |

**One step in: the shelf of tabs**, at the lower left, under the first candidates. One tab is open at a time, or none; which one is the screen's own memory.

| Tab | What it holds | Its heading shows |
| --- | --- | --- |
| Range | Every item of the range in the order it went in (belt.md 7.1), landed and not yet in included. | The number of items. |
| Candidates | Every Issue that could go in, in the scan's order, then every one that cannot, with why; then the projects the scan could not rank (3.1). | Both numbers of Issues ("14 + 5 held"), and the projects apart ("· 1 project"). |
| Other runs | The runs outside any range, stopped ones first. | Their number. |
| Pull requests | Every pull request of a run not closed, failed ones first. | Their number. |
| One task | The progress of one task from dispatch to now, from the events; opened from any row that names a task, and the tab keeps the last one. | Nothing until a task was opened; then its id. |
| Brief | The range's brief, whole, as it was approved. | Nothing. |

Nothing is hidden silently behind a tab. Each heading carries its count, and it changes colour with what is behind it: **red** when something there failed (a launch, an anomaly, a failed CI), else **amber** when something there waits for the person, else plain; beside the count it says how many (`!1`, `?2`). What waits for the person and what failed are also always seen in their own regions; the heading's colour says where the row is, it is never the only sign.

Rules of the look, which the drawing follows:

- A state is never colour alone: each place has a mark, a word and a colour (`✓ landed`, `□ closed`, `? waits for you`, `● in work`, `◆ review`, `○ not yet in`, `– skipped`): the seven places of `ranges[].places`, each drawn, none merged into another. "Failed to start" (`!`, red) is not a place: it is an item in work whose stage is `launch_failed`, marked inside the in-work part (3.3).
- Amber means "waits for the person" and nothing else. Red means "went wrong" and nothing else. Green is work that moves or landed. Everything else is ink on paper.
- An empty list says why it is empty (the panel "nothing could go in"): the scan found nothing, failed, or has not run.
- A list of the range never reorders what has been admitted (belt.md 7.1); a row changes its mark, not its position. "In work now" is the one list ordered by need (stopped first); the fixed order is in the tab Range.
- Every time is an age ("6 min") beside the newest read's age in the top line, so an old screen cannot pass for a fresh one. When the heartbeat is old the screen says so over everything and greys what it last read (the panel "the dispatcher is silent").
- Words are the person's, not the record's: "waits for you" for `waiting_person`, "not yet in" for `not_in`.

### 2.1 When a place is full

The owner drew the first draft with larger counts and it broke (2026-11-23): with twelve candidates a whole section vanished, and of a range of twenty only eight items showed. The rule since: the frame is fixed, and nothing that does not fit may vanish without a mark. With the tabs, the long lists live behind them, so the always-seen regions are capped and the tabs scroll.

- **Capped, ending in a line "and N more"**: an always-seen region. The line says what the rest is and where it is (which tab, or how old and of which kind).
- **Scrolling in its own place**: an open tab, and the two regions whose whole content is their point. The heading carries the full count, and the bar is drawn whenever the content is longer than its place.

| Place | When full | Why |
| --- | --- | --- |
| The range (left) | Never folded. Every other range of `ranges[]` is one line under it (id, project, status, budget used, runs not closed) that selects it: a range of another project, a proposed one, and an ended or replaced one that still has runs not closed (a replaced range keeps its runs, belt.md section 11, and they are not "Other runs"). The selected range is the one the card, the band, "In work now" and the tabs Range and Brief show; its id is in each of their headings. Selected at first: the active range with something waiting for the person or failed, else the active one approved last. A line of a range not selected turns amber or red as a tab's heading does. | One card, and no run that can only be reached through a range the screen does not offer. |
| The first candidates | Three; one when a tab is open and the rows are three lines each (more than one project). Then "and N more, in the tab Candidates, with M that cannot go in yet". | The top of the scan's ranking is what is asked for most; the owner asked to see every one, and every one is one step in. |
| The shelf's headings | Wrap onto a second and third line; never scroll, never fold. | A heading that is not seen cannot turn amber or red. |
| An open tab | Scrolls in the shelf. It is given at least half of the column under the range; the first candidates give way. | A tab is opened to be read. |
| Tab Range | Scrolls; it opens at the first item that has not landed, the landed ones above it. | Its order is fixed, so it cannot be cut by importance. |
| Tab Candidates | One scroll with two headings that stay at the top while their part is in view: could go in, cannot go in yet. | "Why not that one" is looked up now and then; its count is in the heading always. |
| Tabs Other runs, Pull requests, One task, Brief | Scroll. The brief is never cut. | They are read through, not glanced at. |
| Answer: the entry | The card takes at most half of the centre column. The first three lines of the question and the answer buttons stay; the middle (the question whole when it was cut, the background, what each answer does, the reason) scrolls inside the card. | The buttons must never scroll away from the question, and the conversation under it must keep its place. |
| Answer: several entries | One at a time, "1 of 3" and "next"; the list of them is capped at four rows and scrolls. | One is answered at a time. |
| The conversation | Never less than a quarter of the column. | It is the centre today (2.3). |
| Went wrong | Capped at three, newest first except a silent dispatcher, which is always first; then "and N more" with the oldest's age and the count per kind. With more than three, each row's detail is one line. The count in the top line is always the full count. | A long list of failures must not push the range out of view. |
| The range's band | One mark per Issue up to twelve items. Above twelve, one piece per place, as wide as its share, with its number written in it; the in-work piece is drawn as two when some of its items failed to start, the moving ones and the failed ones in red. The counts in words stay under it in both forms. | Twenty marks of a few pixels say less than seven numbers. |
| In work now | Scrolls; it takes the height the column has left, never less than three rows. Stopped ones are first, so what is under the fold is moving. | It is bounded by how many run at once, and its top is what needs a look. |

Widths follow the same rule: nothing is cut without a mark, and nothing wraps over its neighbour.

- A reference is never wrapped and never given a fixed width: its cell is as wide as its text. A title beside it keeps at least sixteen characters before it is cut with an ellipsis, and a cut title shows whole on hover.
- With one project registered a reference is `#N`, in a cell before the title. With more than one it is `OWNER/NAME#N`, too long to sit before a title in a column of this width: it goes on a line of its own under the title, whole, in every list; one longer than the row is cut at its start, never its end, so the number always stays, and shows whole on hover. The form is chosen by the number of projects, not row by row, so the rows of one list have one shape.
- A state at the right of a row ("waits for you · 6 min") is never cut; the title gives way first.
- A label longer than the drawing's English (another language) wraps inside its own cell or is cut with an ellipsis; no layout depends on a label's length. The section headings, the buttons and the top line are one line each and cut the least important part first (the explanation after a heading, then the organization's name).

### 2.2 Language and light or dark

- **Language.** The screen's wording switches with the display-language setting, from the top line. All wording is kept in one place: one table per language, keyed by what is said (a place, a stage, a reason, an event kind, a button, a heading), as `roles/locale/` does for the sentences of spec section 13; no view holds a sentence of its own, so a language is added by adding a table and the layout is tested with the longest of them. A key with no wording in the chosen language is shown in English, and one with none at all by its record name, never dropped. What comes from the records is shown as written: titles, questions, reasons, the secretary's terminal. The setting starts from `conversation_language` (spec section 13); neither output prints the language in force (the setting, or `makibad`'s environment when unset), which is missing (section 6), so until then it starts from the environment of the screen's own process, read as spec section 13 reads it. The choice is the screen's own and changes nothing in the organization. The drawing is in English only: the repository's language check allows no other text under `docs/`, and the owner checks a long Japanese wording against the layout on their side.
- **Light or dark.** Three choices in the top line: follow the system (the default), light, dark. Every colour is a named value with one light and one dark figure; no part has a colour of its own. The meanings do not change: amber waits for the person, red went wrong, green moves or landed. The conversation is dark in both.
- Both switches are drawn in the drawing and not wired, as the tabs are: it carries no script. The dark variant is its third picture.

### 2.3 The centre is a judgement of the present

The conversation is in the centre because today asking the secretary is what the person does most (the owner, 2026-11-22). That is a judgement of the present, not a property of the screen: as makiba runs more of its own work (the belt, the stops it answers as recommended), the person may talk less and watch more, and the weight of the centre may go to the range or to what waits (the owner, 2026-11-23).

So each part is built as a region of its own: the top line, the range, the first candidates, the answer, the conversation, "Went wrong", the range's band, "In work now", the shelf and each of its tabs. A region reads its own part of the model (section 3), draws itself at whatever width and height it is given, and knows nothing of its neighbours: no region reaches into another, and none assumes its column. Which region sits where is written in one place, the layout, and nowhere else. Moving the conversation to the side, or the range to the centre, is then a change to that one place.

No mechanism to rearrange is built: no dragging, no setting for the order, no saved layouts. The layout is code, changed by a commit when the judgement changes.

## 3. Each part and its read field

The screen opens no table and no file. It reads `makiba status --json` on an interval and `makiba events --json --after <id>` for the trail (spec section 12). "Today" means the field is on the read surface now; "missing" means the part cannot be drawn until the field exists, and section 6 lists those.

The tables below are by role; where a part is drawn is section 2. "Could go in now" feeds the first candidates and the tab Candidates, which also holds "Cannot go in yet"; the range's rows feed "In work now" and the tab Range; "Runs outside a range" is the tab Other runs; "The trail of one run" is the tab One task; the brief head is in the approval and in the tab Brief. A tab's count and colour are computed from the same rows as its content, never kept apart from it.

### 3.1 Put in

| Part | Read field | State |
| --- | --- | --- |
| The range: id, project, status, terms | `ranges[].id`, `.project`, `.status`, `.issues`, `.takes`, `.at_once`, `.approval_mode` | Today |
| Budget used | `ranges[].budget.tasks`, `.used.tasks` | Today (hours and fixes are belt step 2) |
| Approved when | `ranges[].approved_at` | Today |
| A proposed range and its read | `ranges[].candidates[]` (`issue`, `title`, `admitted`, `left_out_why`), `.read_id`, `.read_at`, `.read_complete` | Today |
| What every task of the range starts from | `ranges[].brief_head`, whole, never cut: the approval covers it (belt.md section 1, "Approve"), so a proposed range shows it in full, scrolling inside its box, before the button can be pressed | Today |
| Could go in now | `next_work.candidates[]`: `repo`, `issue`, `title`, `clause_id`, `size`, `size_estimated`, `why`, `recommended` | Today |
| Cannot go in yet | `next_work.left_out[]` with an `issue`: `repo`, `issue`, `kind`, `reason`; `candidates_not_recorded` and `left_out_not_recorded`, both numbers, shown as "and N more not listed" | Today (a left-out entry has no title: missing) |
| Projects the scan could not rank | `next_work.left_out[]` with a null `issue`: `project`, `repo`, `kind`, `reason`. A whole repository (`no_goal_ledger`, `goal_ledger_error`) is named by its project and repository; `no_repository` has a null `repo` too and is named by its project alone; an entry with no project is named by its repository. They are listed under their own heading in the tab Candidates, "Projects not ranked", after the Issues, and are not counted as held Issues: the tab's heading reads "14 + 5 held" for Issues and adds "· 1 project" | Today |
| The scan read only part | `next_work.input_truncated`, a list of the reads cut short (`open_issues`, `open_prs`, `base_merges`), not a count: shown as its own warning, "the scan did not read every open Issue", naming the reads | Today |
| Why the list is empty | `next_work` null (no scan yet), `.status` (`no_candidates`, `error`), `.error`, `.scanned_at`, `.trigger` | Today |
| A scan recorded before the lists were kept | `next_work.candidates` and `.left_out` null while `.candidate_count` is above 0 (a scan older than Issue 256): shown as "N found; their details were not recorded, the next scan will list them", never as "nothing could go in" | Today |

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
| The strip and the counts | `ranges[].places`, as it is: one count per place, all seven (`landed`, `closed`, `waiting_person`, `review`, `in_work`, `not_in`, `skipped`), in that order. `closed` (completed with no merge recorded) is shown apart from `landed`, with the mark `□`, the word "closed" and "no merge recorded" in its row: a close is not a landing (belt.md 7.1) | Today |
| "Failed to start" inside the in-work count | Not a place and not in `places`: the items of `ranges[].items[]` with `place` `in_work` and `stage` `launch_failed`, counted by the screen. Shown as "3 in work (1 failed to start)", the red part of the in-work piece of the strip and a red `!` on the row; the same run is in "Went wrong" | Today |
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
| Ask for this | `send secretary <text>` | Person | A fixed sentence naming the repository and the number only: "Please take up OWNER/NAME#N." No title: the secretary reads the Issue. The secretary decides how; the screen dispatches nothing. |

"Ask for this" is a message and not a `dispatch` so that the secretary stays the one contact (goal G1) and the brief stays the secretary's work. A new range is drafted in the conversation (belt.md section 1); the screen has no form for one.

This is a trust boundary, and it is designed separately before any of it is built: a program other than the person's shell would send requests with the person key. The questions that design must answer: who holds the key (the screen's own process reading `person.key` as `makiba` does, or the screen running the `makiba` binary); how a page's requests are told from another local program's (a page on 127.0.0.1 can be asked by anything on the machine, an agent included, unless its sandbox has no route there); and that a click is recorded as the person's act through the screen (`commands.actor`). Until that design is agreed, the screen is read-only and each control shows the line to type instead (`makiba answer 14 yes`), which the person runs in their shell.

A line to type is an output that ends in the person's shell, so it never carries record text: no title, no question, no reason, nothing that came from GitHub or from an agent. It is built from the verb, numbers (a pending id, a range id, an Issue number) and the repository as `OWNER/NAME`, taken from `projects[].repo` and shown only when it matches the characters GitHub allows in an owner and a name (letters, digits, `-`, `_`, `.`); a repository that does not match gets no line, and the control says so. Any value that must appear is one single-quoted shell word, with a single quote inside it written as `'\''`. The note of an answer is typed by the person in their shell, not carried by the line. The same rule holds for a door request once the screen acts: the fixed sentence of "Ask for this" carries the repository and the number, never the title.

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
| `conversation_language`, as resolved | Where the display language starts (2.2) | It starts from the environment of the screen's own process; the switch in the top line overrides it. |
| `events --task <task_id>` | The trail of an old run | The screen keeps what it has read; a run older than its memory is read again from event 0. |
| `closed_today[]`, or runs closed since a time | "Closed today" | Folded from `run_closed`. |

The rule behind the list: the screen states nothing `makibad` could state. Folding events in the screen is accepted for the trail, which is the events, and is a stopgap everywhere else.

## 7. The order of building

Nothing starts before the owner agrees to the drawing.

| Step | What | Needs | Declared test |
| --- | --- | --- | --- |
| 0 | The drawing agreed; the measurement of section 5 run and recorded. | The owner | none (documents) |
| 1 | The model: from `status --json` and `events --json` to the parts of section 3. Read-only. | Step 0; the read surface as it is | `tests/screen_model.rs`: from a recorded status with one escalated entry, one failed launch and a range of mixed places, the model gives the entry with its context, "went wrong" with the failed launch, the range's rows in the read's order, and the runs with the stopped ones first; from a status with `next_work` null, in error, with no candidates, and with a positive count but null lists, four different lines for the empty list; a failed run shown as "launch failed" with no reason (the reason is asserted only once `runs[].launch_failed_reason` exists, section 6) and a suspended run not shown as gone wrong; with two projects, every reference as `OWNER/NAME#N`; a range whose `places` holds a `closed` item and an in-work item of stage `launch_failed`: the counts are those of `places` with `closed` on its own, and the in-work count carries "1 failed to start" without leaving the in-work total; the headings of the tabs (each with its count; red when something behind it failed, else amber when something behind it waits for the person, else plain, with how many); "In work now" as the range's items that are failed to start, waiting, at work or in review, stopped first; every line to type, built from fixtures whose Issue titles, questions and reasons hold a double quote, a single quote, a backtick and `$(touch <a file>)`: no line contains any of that text, and each line, run by `sh` with a `makiba` that only records its arguments, creates no file and passes exactly the verb, the numbers and the repository; a repository name outside the allowed characters gives no line; a replaced range that still has a run not closed: it is offered, selecting it gives its band, its items and its brief, and its run is not among "Other runs"; `left_out` entries with a null `issue` (a whole repository, and `no_repository` with a null `repo`): named by project, listed apart, and absent from the count of held Issues; the caps of 2.1 (three of six things gone wrong and "3 more", the strip as numbers above twelve items); and every wording read from the one table of 2.2, with a missing key shown by its record name. |
| 2 | The frame chosen in step 0 (the page behind `makiba screen`, or the app's), read-only, each control showing the line to type. | Step 1 | Depends on the frame; for the page, a test that it answers on 127.0.0.1 only and serves what the model gives. |
| 3 | The missing read fields of section 6, each its own Issue. | - | `tests/read_surface.rs` grows. |
| 4 | Acting from the screen (section 4), after its own design and the owner's agreement. | M6-1; the boundary design | Its own. |

## 8. Not in the first version

A notification that calls the person (Issue 272). The organization's own state: capacity, the base, the versions (the owner, 2026-11-22). The screens of workers (the person is never called to a terminal; `peek` stays the secretary's). Several organizations or several people on one screen (belt.md section 10: the fields allow it, the layout does not try). A phone-sized layout.
