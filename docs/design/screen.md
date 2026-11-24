# The screen: where the person puts work in, watches it and answers

Status: design note for Issue 169, 2026-11-22; redrawn twice on 2026-11-23 after the owner's readings (what each place does when it is full, 2.1; the language and light or dark, 2.2; what is always seen and what is behind a tab, 2; the centre as a judgement of the present, 2.3; the words on the screen, 2.4; who is handling what went wrong and what can be done, 4.1). Refined on 2026-11-24 for Issue 394 against what the read surface gives today: a visual system (2.5), an inventory of every part against `makiba status --json` and `makiba events --json` (3.5), a fixture of the read surface ([screen-fixture/](screen-fixture/)), the drawing redrawn from that fixture only, the gaps of section 6 brought up to date, and the questions left to the owner (section 9). Nothing is built. The drawing is [screen-draft.html](screen-draft.html): a static page with no script, no request and no font fetched, every value read off the fixture, a missing field drawn as a dashed box that names it; open it in a browser 1440 px wide or wider. Redrawn three times the same day (rounds 2, 3 and 4, section 2) after the owner's readings. It holds the fixture's day with four repositories: light at 1440 × 900 with all repositories shown, dark with the repository loom selected (the screen filtered to it), the narrow width (1024 × 720), eight panels (the views "Pull requests" and "Failures" over every repository, "Issues to start" filtered to loom, a proposal to approve, "One-off tasks", "Task history", the third question, and the empty, loading, error and stale states), and the visual system in both themes. The owner agrees to the drawing before any code (the Issue). Read beside [belt.md](belt.md) (sections 1 and 7), [spec.md](spec.md) (section 12, the read surface) and the runbook [herdr-app.md](../runbooks/herdr-app.md). A design review (one round) was folded in: the repository in every match and request (3.1, 4), the brief head in the approval (3.1), a failed run told from a suspended one (3.3), concluded pull request watches (3.3), approving the read that was shown (4), the language (2) and the truncated scan (3.1).

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

**Round 2 of the layout (Issue 394, the owner's reading of 2026-11-24).** The first drawing of Issue 394 was read and sent back: the black terminal box took the best place while showing nothing; there was so much red that what is really urgent could not be told; the shelf "More" was a pile of buttons in which a switch could not be told from a state; the text was small and cut in many places, and the three columns had one density, so nothing led; boxes inside boxes, rounded badges and coloured tags made it look dated. The owner asked for structure shown by whitespace and the weight of the type, in the direction of today's issue trackers and code hosts (a direction, not a copy). Where this paragraph and the tables below differ, this paragraph holds; the regions themselves (2.3) and what they read (section 3) do not change.

- **Left: a sidebar that only navigates** (248 px, on `bg`). Under "Approved work", one entry per range of `ranges[]` (the selected one first, then those with something waiting or failed); under "Views", the former tabs: Failures, Issues to start, One-off tasks, Pull requests, Task history. Every entry has one look: its name, its count on the right in plain figures, and, only when there is something, a second line of words in the state's colour ("3 unattended", "1 needs you · 1 did not start"). The selected entry is a white strip. A switch is therefore always a name in the sidebar, and a state always a sentence under it. "All approved items" and "Instructions" are no longer tabs: selecting a range shows its items, and its instructions are a link in its heading.
- **Centre: what leads.** At the top, what waits for the person: the question itself as the largest text on the screen (20 px), the kind and the task over it in one quiet line, if yes and if no side by side, the recommended one marked by a green rule and the word "recommended", the buttons under them. Under it, the view the sidebar selected, by default the selected range: its heading, its progress as a thin bar of twelve cells with its budget in words beside it, and its items grouped by what they need (needs you, did not start, in review, being worked on), each group a small heading with its count; the finished groups (merged, closed, not started, skipped) are folded into one line of links. This replaces the band and "Active now" of the right column, and the shelf: a view is one step in, as a tab was, but it opens where there is room to read it.
- **The conversation, folded.** One line at the foot of the centre: the secretary's state and age, how many questions are still with it, and "Open the conversation". The read surface holds no conversation, so the place shows nothing until the screen runs beside the secretary's terminal (section 5); there, opening it gives the terminal the lower half of the centre. This changes the judgement of 2.3 only in size: the conversation stays in the centre, folded until it can show something.
- **Right: watch, narrow** (360 px). "Failures" by degree (4.1 and 2.5): the rows that stopped with nobody on them first, at most two, then a fold line that says what the rest are ("1 waits for your answer · 1 probably ended"); then "Ready to start", three Issues with "Ask to start", and a fold line.
- **Top line.** Plain words with the number in the state's colour and the mark before it ("2 need you", "4 failed · 3 unattended"), no pills; the display switches are words, the chosen one in bold.
- **Narrow (1024 px).** The right column goes; "Failures" and "Issues to start" are reached from the sidebar, whose entries keep their counts and their red or amber words, and the top line keeps both counts.

The question Q4 of section 9 (room for an open tab) is answered by this layout: a view opens in the centre, not under the candidates.

**Round 3 of the layout (Issue 394, the owner's reading of 2026-11-24, entry 38): several repositories at once.** A project has at most one active range (belt.md), so with several projects registered several ranges run at the same time, and Issue and pull-request numbers repeat across repositories. The fixture now holds four projects (three with a range in progress, one with a proposal) and two collisions on purpose (`loom#56` and `quill#56`; the pull requests `loom#127` and `harbor#127`). What changes:

- **References: always `project#N`.** Every Issue and pull request on the screen is written with its project's slug: `loom#56`, `harbor#127`, `atlas#7`, in mono, the slug in ink and the number in grey, whatever the number of projects and whatever the filter. The slug is the name the person gave the project with `makiba project`; it is unique in the organization, short, and exists for a project with no repository recorded too, so one form covers every case (the form `<project>#N` of 3.1 becomes the only form). `OWNER/NAME` is shown in the heading of a repository's view and on hover; a line to type and a message to the secretary still carry `OWNER/NAME#N` (section 4), since GitHub knows no slug. This replaces the rule of 2.1 (`#N` with one project, `OWNER/NAME#N` on a line of its own with more) and withdraws the recommendation of Q2.
- **The sidebar lists repositories first.** "All repositories", then one entry per project in a fixed order by name (an order that never moves under the hand), each with its range's number and status, its tasks used of its budget, and, only when there is something, a line of words: "1 needs you · 3 failed", "needs your approval". Under it, "Views", whose counts are those of the selected repository.
- **Choosing a repository filters the screen.** Its range is the centre's view, and every list (failures, ready to start, the views of the sidebar) shows that repository only; the region's heading says so ("3 in loom") and one click goes back to all. The top line never filters.
- **"All repositories" is the default view.** One block per repository, in the sidebar's order: its slug, its range's status and progress, at most the two rows that need a look (needs you, did not start, an error), and one quiet line for the rest with "Open". A proposal is a block whose row is "Review the proposal".
- **The top line names where.** The totals stay, followed by where they are: "3 need you in atlas, loom, quill", "6 failed · loom 3 · harbor 2 · atlas 1 · 5 unattended"; each name filters to that repository. The display switches fold into "Display: English · Light" to leave the line its room (2.1).
- **A failure of the whole organization is never filtered out.** A row with no project (makibad silent or running an older program than the one installed, the secretary gone or still, a failed scan) is headed "Whole organization" instead of a reference, comes first in "Failures", is counted as "organization N" in the top line, and is shown under every repository's filter ("3 in loom, 1 for all").
- **A failure row has one coloured line.** The kind and who is on it are one sentence: "Did not start · no sign that anyone is on it", in red; the detail and the actions under it are grey or links ("Abandon and close…" is no longer red: the confirmation says what it does).

**After the diff review (round 2 of the review, 2026-11-24).** Three gaps in the layout of rounds 2 to 4, fixed:

- **A repository with two ranges.** A project has at most one active range, but a proposed range may wait beside it, replacing it once approved (`range_proposed.replaces`; a replacing approval ends the old range). The sidebar entry stays one per repository and says so in its state line ("#7 proposed", "needs your approval"); the repository's block in "All repositories" shows the proposal as its first row with "Review the proposal" and "approving it ends #6"; selecting the repository shows the active range, with the proposal as a row above its groups. The fixture holds it: range 7 of `harbor`, proposed to replace range 6.
- **The order things went in is kept.** "All approved items" was a tab with the range's every item in a fixed order (belt.md 7.1); rounds 2 to 4 left only the grouping by need. A range's view now has a switch, "By need · In order": "In order" lists every item in the order it went in, each row changing its mark, never its position (the drawing's panel "In order"). By need stays the default, since it is what calls for the person.
- **What the read surface cannot say is not said.** A notice to the secretary is shown as written ("a notice was written to the secretary", from a `message_sent` of `makibad` on the task), never as told or delivered, which is D of section 6.

**Round 4 (the owner's reading of 2026-11-24, entry 39): no word alone on a line.** The round-3 drawing broke text in odd places: titles in the right column dropped only their last word to a second line ("Keep the last five / releases"), the state lines of the sidebar broke inside a phrase ("1 needs / you"), and the last row of the centre went under the secretary's line. What changes:

- **In the right column the reference is a line of its own** (`harbor#15`, mono), and the title is the next line, whole, at the column's full width; the title, the coloured line and the meta are set with the browser's balanced wrapping (`text-wrap: balance` for titles, `pretty` for running text), so a short last line is avoided where a line must break. The column keeps its 360 px: the centre leads, and the titles of the fixture fit on one line at that width once the reference is off it.
- **A state line under a sidebar entry wraps only between its phrases** ("needs your approval", "1 needs you", "1 failed" are each kept whole; the separator goes with the phrase after it, never at the end of a line).
- **The centre draws only whole rows, above the secretary's line, never under it.** A row or a repository's block that does not fit whole is not drawn in part: the list ends with a fixed strip that names what is below with its state ("Below: quill 1 needs you"; in a repository's view, its groups as links: "In review 3 · Being worked on 1 · Merged 2 …"). The right column does the same: a failure or a candidate that does not fit whole goes into the fold line under it. Nothing is hidden without being named (2.1).
- **A fold line and a coloured line wrap only between phrases** ("1 stopped (loom, 17 h)", "1 waits for you", "1 probably ended"; "Its terminal is gone", "no sign that anyone is on it"), the separator going with the phrase after it.
- **A reference and the word before it stay together** where a sentence ends on one ("pull request loom#131", "Depends on #<n>").
- **Checked on the rendered page, not by eye.** A check run over the drawing as Chromium lays it out finds, for every block of text, its lines from the position of each word, and reports any block of two lines or more with a line that holds a single word. Run on the round-3 drawing it reports 21 such lines, the two titles the owner named among them; on this drawing, over 722 blocks (every value of the fixture that is drawn, in all three pictures and every panel), it reports none. The same run also reports any row, block heading, summary or fold line that crosses the bottom of its list or of the right column, or is drawn under the strip: none. The check is a script run on a copy of the page; the drawing itself still has no script.

**Always seen**

| Region | What it holds | Where, and why there |
| --- | --- | --- |
| Top line | The organization's name; "Needs you N"; "Failed N" with "unattended N"; whether `makibad` is alive and the age of the read; the two switches of 2.2. | The two counts are the only things that call for the person. |
| The range | The selected range: its terms in one line, the budget used, pause and end; a proposed range with the read to approve. Every other range of `ranges[]` as a line under it (2.1). | Left, "Put in": putting in is the most frequent act, and reading starts at the left. |
| The first candidates | The first three Issues that could go in now, with "Ask to start", and a line that counts the rest and names the tab that holds them. | Left, under the range. |
| What waits for the person | One entry at a time: a one-line heading, the question once, the background folded (2.1). | Centre, over the conversation: a question arrives in the conversation too (belt.md section 1, "Answer"), so the entry and the conversation it came from are seen together. With nothing waiting, the conversation takes the whole column. |
| The conversation | The secretary's terminal, as it is. | Centre (2.3). |
| Failures | Only when there is something. Each row says who is handling it and carries what can be done about it (4.1). | Right, "Watch", at the top. |
| The range's band | The strip and the count per place (the budget is with the range, on the left). | Right, under "Failures". |
| Active now | The range's items that are neither finished nor waiting to go in: failed to start, waiting for the person, at work, in review; stopped ones first. | Right, the rest of the column. The person decides nothing here (the owner: its purpose is reassurance), so it sits at the side and asks for no click. |

**One step in: the shelf of tabs**, at the lower left, under the first candidates. One tab is open at a time, or none; which one is the screen's own memory.

| Tab | What it holds | Its heading shows |
| --- | --- | --- |
| All approved items | Every item of the range in the order it went in (belt.md 7.1), landed and not yet in included. | The number of items. |
| Issues to start | Every Issue that could go in, in the scan's order, then every one that cannot, with why; then the projects the scan could not rank (3.1). | Both numbers of Issues ("14 · not ready 5"), and the projects apart ("· 1 project unchecked"). |
| One-off tasks | The runs outside any range, stopped ones first. | Their number. |
| Pull requests | Every pull request of a run not closed, failed ones first. | Their number. |
| Task history | The progress of one task from dispatch to now, from the events; opened from any row that names a task, and the tab keeps the last one. | Nothing until a task was opened; then its id. |
| Instructions | The range's brief, whole, as it was approved. | Nothing. |
| Failures | Every row of "Failures" with its state and controls (4.1); it is where "and N more" leads. Not on the shelf while nothing went wrong. | Their number, always red, and how many have nobody acting ("6 · unattended 2"). |

Nothing is hidden silently behind a tab. Each heading carries its count, and it changes colour with what is behind it: **red** when something there failed (a launch, an anomaly, a failed CI), else **amber** when something there waits for the person, else plain; beside the count it says how many, in words ("1 failed", "2 need you"), never as a sign that needs a legend. What waits for the person and what failed are also always seen in their own regions; the heading's colour says where the row is, it is never the only sign.

Rules of the look, which the drawing follows:

- A state is never colour alone: each place has a mark, a word and a colour (`✓ merged`, `□ closed, not merged`, `? needs you`, `● being worked on`, `◆ in review`, `○ not started`, `– skipped`; the words are those of 2.4): the seven places of `ranges[].places`, each drawn, none merged into another. "Did not start" (`!`, red) is not a place: it is an item in work whose stage is `launch_failed`, marked inside the in-work part (3.3).
- Amber means "waits for the person" and nothing else. Red means "went wrong" and nothing else. Green is work that moves or landed. Everything else is ink on paper.
- An empty list says why it is empty (the panel "nothing could go in"): the scan found nothing, failed, or has not run.
- A list of the range never reorders what has been admitted (belt.md 7.1); a row changes its mark, not its position. "Active now" is the one list ordered by need (stopped first); the fixed order is in the tab "All approved items".
- Every time is an age ("6 min") beside the newest read's age in the top line, so an old screen cannot pass for a fresh one. When the heartbeat is old the screen says so over everything and greys what it last read (the panel "the dispatcher is silent").
- Words are the person's, not the record's: "needs you" for `waiting_person`, "not started" for `not_in`; every one of them is in the table of 2.4.

### 2.1 When a place is full

The owner drew the first draft with larger counts and it broke (2026-11-23): with twelve candidates a whole section vanished, and of a range of twenty only eight items showed. The rule since: the frame is fixed, and nothing that does not fit may vanish without a mark. With the tabs, the long lists live behind them, so the always-seen regions are capped and the tabs scroll.

- **Capped, ending in a line "and N more"**: an always-seen region. The line says what the rest is and where it is (which tab, or how old and of which kind).
- **Scrolling in its own place**: an open tab, and the two regions whose whole content is their point. The heading carries the full count, and the bar is drawn whenever the content is longer than its place.

| Place | When full | Why |
| --- | --- | --- |
| The range (left) | Never folded. Every other range of `ranges[]` is one line under it (id, project, status, budget used, runs not closed) that selects it; at most three such lines, those with something failed or waiting first, then "and N more ranges", which opens a list of them all that scrolls in the place of the first candidates until one is selected. The lines are for: a range of another project, a proposed one, and an ended or replaced one that still has runs not closed (a replaced range keeps its runs, belt.md section 11, and they are not "One-off tasks"). The selected range is the one the card, the band, "Active now" and the tabs "All approved items" and "Instructions" show; its id is in each of their headings. Selected at first: the active range with something waiting for the person or failed, else the active one approved last. A line of a range not selected turns amber or red as a tab's heading does. | One card, and no run that can only be reached through a range the screen does not offer. |
| The first candidates | Three; one when a tab is open and the rows are three lines each (more than one project). Then "and N more, in the tab "Issues to start", with M that cannot go in yet". | The top of the scan's ranking is what is asked for most; the owner asked to see every one, and every one is one step in. |
| The top line | One line. The two counts ("Needs you N", "Failed N · unattended N") are two words and a number each, never shortened, never cut and never off the first line. What gives way, in this order: the organization's name is cut with an ellipsis, then dropped; the two switches fold into one control, "Display"; "Running · updated 3 s ago" becomes "Running · 3 s"; the name "makiba" is dropped. If the line still does not fit, it wraps: the two counts on the first line, the rest on the second. "unattended N" is shown only when N is above 0. | The counts are what calls the person; everything else in the line can be found again. |
| The shelf's headings | Wrap onto a second and third line; never scroll, never fold. | A heading that is not seen cannot turn amber or red. |
| An open tab | Scrolls in the shelf. It is given at least half of the column under the range; the first candidates give way. | A tab is opened to be read. |
| Tab "All approved items" | Scrolls; it opens at the first item that has not landed, the landed ones above it. | Its order is fixed, so it cannot be cut by importance. |
| Tab "Issues to start" | One scroll with two headings that stay at the top while their part is in view: could go in, cannot go in yet. | "Why not that one" is looked up now and then; its count is in the heading always. |
| Tabs One-off tasks, Pull requests, Task history, Instructions, Failures | Scroll. The brief is never cut. | They are read through, not glanced at. |
| Answer: the entry | The card always leaves the conversation more than half of the centre column: it takes at most 45% of it. Its heading is one line and never more: what kind of entry it is and the task's title ("A decision · A screen for the person"), cut with an ellipsis. The question is written exactly once, whole, under the heading; nothing repeats it. The background is folded by default to one line ("Background, 3 paragraphs") and opens in place on demand; what each answer does and the recommendation with its reason are shown. The heading and the answer buttons stay; everything between them scrolls inside the card when it is longer than the cap, with the background open or a long question. | The owner's reading of the second draft (2026-11-23): the question was shown twice (cut in the heading, whole under it) and the card took half the column from the conversation. The buttons must never scroll away. |
| Answer: several entries | One at a time, "1 of 3" and "next"; the list of them is capped at four rows and scrolls. | One is answered at a time. |
| The conversation | Never less than a quarter of the column. | It is the centre today (2.3). |
| Failures | Capped at three, a silent dispatcher first, then the rows with nobody on them, then the others, newest first within each; then "and N more" with how many of them have nobody acting and the oldest's age. That line opens the tab "Failures" on the shelf: every row, the three included, each with its state and its controls (4.1), scrolling; a row with no task (a failed scan, a silent dispatcher) is there as any other. The count in the top line is always the full count. | A long list of failures must not push the range out of view, and no row with something to do about it may be out of reach. |
| The range's band | One mark per Issue up to twelve items. Above twelve, one piece per place, as wide as its share, with its number written in it; the in-work piece is drawn as two when some of its items failed to start, the moving ones and the failed ones in red. The counts in words stay under it in both forms. | Twenty marks of a few pixels say less than seven numbers. |
| Active now | Scrolls; it takes the height the column has left, never less than three rows. Stopped ones are first, so what is under the fold is moving. | It is bounded by how many run at once, and its top is what needs a look. |

Widths follow the same rule: nothing is cut without a mark, and nothing wraps over its neighbour.

- A reference is never wrapped and never given a fixed width: its cell is as wide as its text. A title beside it keeps at least sixteen characters before it is cut with an ellipsis, and a cut title shows whole on hover.
- Superseded by round 3 (section 2): every reference is `project#N`, in its own column before the title. What follows was the rule before it. With one project registered a reference is `#N`, in a cell before the title. With more than one it is `OWNER/NAME#N`, too long to sit before a title in a column of this width: it goes on a line of its own under the title, whole, in every list; one longer than the row is cut at its start, never its end, so the number always stays, and shows whole on hover. The form is chosen by the number of projects, not row by row, so the rows of one list have one shape.
- A state at the right of a row ("needs you · 6 min") is never cut; the title gives way first.
- A label longer than the drawing's English (another language) wraps inside its own cell or is cut with an ellipsis; no layout depends on a label's length. The section headings, the buttons and the top line are one line each and cut the least important part first (the explanation after a heading, then the organization's name).

### 2.2 Language and light or dark

- **Language.** The screen's wording switches with the display-language setting, from the top line. All wording is kept in one place: one table per language, keyed by what is said (a place, a stage, a reason, an event kind, a button, a heading), as `roles/locale/` does for the sentences of spec section 13; no view holds a sentence of its own, so a language is added by adding a table and the layout is tested with the longest of them. A key with no wording in the chosen language is shown in English, and one with none at all by its record name, never dropped. What comes from the records is shown as written: titles, questions, reasons, the secretary's terminal. The setting starts from `conversation_language` (spec section 13); neither output prints the language in force (the setting, or `makibad`'s environment when unset), which is missing (section 6), so until then it starts from the environment of the screen's own process, read as spec section 13 reads it. The choice is the screen's own and changes nothing in the organization. The drawing is in English only: the repository's language check allows no other text under `docs/`, and the owner checks a long Japanese wording against the layout on their side.
- **Light or dark.** Three choices in the top line: follow the system (the default), light, dark. Every colour is a named value with one light and one dark figure; no part has a colour of its own. The meanings do not change: amber waits for the person, red went wrong, green moves or landed. The conversation is dark in both.
- Both switches are drawn in the drawing and not wired, as the tabs are: it carries no script. The dark variant is its second picture, and the visual system is drawn in both themes at its end.

### 2.3 The centre is a judgement of the present

The conversation is in the centre because today asking the secretary is what the person does most (the owner, 2026-11-22). That is a judgement of the present, not a property of the screen: as makiba runs more of its own work (the belt, the stops it answers as recommended), the person may talk less and watch more, and the weight of the centre may go to the range or to what waits (the owner, 2026-11-23).

So each part is built as a region of its own: the top line, the range, the first candidates, the answer, the conversation, "Failures", the range's band, "Active now", the shelf and each of its tabs. A region reads its own part of the model (section 3), draws itself at whatever width and height it is given, and knows nothing of its neighbours: no region reaches into another, and none assumes its column. Which region sits where is written in one place, the layout, and nowhere else. Moving the conversation to the side, or the range to the centre, is then a change to that one place.

No mechanism to rearrange is built: no dragging, no setting for the order, no saved layouts. The layout is code, changed by a commit when the judgement changes.

### 2.4 The words on the screen

A name on the screen says what happens when it is pressed, or what the list under it holds (the owner, 2026-11-23: "Open the task" did not say that it shows a task's progress, "Give up…" did not say that it abandons and closes, and "range" and "runs outside the range" said nothing at a glance). Three rules:

- A button is a verb with its object and, where it matters, its effect: "Show its history", "Abandon and close…", "Hold new tasks", "End after running tasks", "Ask the secretary to look". A button that sends something to the secretary says so; one that only moves inside the screen starts with "Show".
- A heading names what is listed in the person's words, with its count.
- makiba's own terms stay off the screen: range, run, place, stage, scan, belt, read, anomaly, dispatch, launch, brief, candidate, pending entry, `makibad`. This note keeps them, because the records are named so; the table below is the one place that ties each word on the screen to the record it shows. What comes from the records is shown as written and is not translated: titles, questions, reasons, a condition such as `no-label:backlog`, the secretary's own words in its terminal.

The table is the English column of the wording table of 2.2: one key per thing said, one row per key, and no view holds a word that is not in it. The Japanese column is the owner's; for now it is kept in a comment on Issue 169, not in this repository, whose language check allows no Japanese under `docs/`. When the screen is built, the Japanese wording goes into the repository as the wording file of that display language, beside the English one, and how the language check treats those files is designed then.

| Key | English | What it means (the record) | Where shown | Japanese: kept in a comment on Issue 169 until the wording files exist |
| --- | --- | --- | --- | --- |
| `top.needs_you` | Needs you N | Questions escalated to the person (`pending[]`, `escalated`) | Top line | |
| `top.failed` | Failed N | Rows of "Failures" (open `anomalies[]`, failed launches, a failed scan, a silent `makibad`) | Top line | |
| `top.unattended` | unattended N | Failures with nobody on them (4.1) | Top line, tab "Failures" | |
| `top.running` | Running · updated N s ago | `makibad.running`, and the age of the screen's newest read | Top line | |
| `top.stopped` | makiba has not responded for N min | `makibad.heartbeat_at` older than 30 s | Top line, banner | |
| `top.display` | Display | The language and light-or-dark switches, folded into one control when the line is short | Top line | |
| `col.hand_over` | Hand over work | The role "put in" | Left column | |
| `col.progress` | Progress | The role "watch" | Right column | |
| `col.more` | Views | The sidebar's list of views (formerly the shelf of tabs, section 2, round 2) | Sidebar | |
| `range.title` | Approved work #N | A range (`ranges[].id`) | Range card, band, tabs | |
| `range.in_progress` | in progress | `ranges[].status` `active` | Range card | |
| `range.proposed` | needs your approval | `ranges[].status` `proposed` | Range card | |
| `range.paused` | on hold | `ranges[].status` `paused` | Range card | |
| `range.ended` | ended | `ranges[].status` `ended`, with `ended_reason` | Range card | |
| `range.at_a_time` | N at a time | `ranges[].at_once` | Range card | |
| `range.budget` | N of M tasks started | `ranges[].used.tasks`, `.budget.tasks` | Range card | |
| `range.hold` | Hold new tasks | `range pause`: nothing new starts, running tasks go on | Range card | |
| `range.resume` | Start new tasks again | `range resume` | Range card | |
| `range.end` | End after running tasks | `range end`: nothing new starts, running tasks finish | Range card | |
| `range.approve` | Approve exactly this | `range approve` with the read shown | Proposal | |
| `range.change` | Ask to change it | Moves to the conversation | Proposal | |
| `range.would_start` | It would start, in this order | `ranges[].candidates[]` admitted, of the read shown | Proposal | |
| `range.not_now` | not now | `ranges[].candidates[].left_out_why` | Proposal | |
| `ready.title` | Ready to start | `next_work.candidates[]` | Left column, tab "Issues to start" | |
| `ready.not_approved` | N Issues, not yet approved | Candidates outside every range | Left column | |
| `ready.ask` | Ask to start | `send secretary` with the fixed sentence naming the repository and the number | Each ready Issue | |
| `ready.suggested` | suggested first | `next_work.candidates[].recommended` | Each ready Issue | |
| `ready.no_goal` | no goal | `clause_id` null | Each ready Issue | |
| `ready.size` | size S / M / L (estimate) | `size`, `size_estimated` | Each ready Issue | |
| `ready.none` | No Issue is ready to start | `next_work.status` `no_candidates` | Left column | |
| `ready.check_failed` | The last check of GitHub failed | `next_work.status` `error` | Left column, "Failures" | |
| `ready.no_check` | No check has been made yet | `next_work` null | Left column | |
| `notready.title` | Not ready yet | `next_work.left_out[]` with an Issue | Tab "Issues to start" | |
| `notready.waits` | Waits for another Issue | `kind` `blocked` | Tab "Issues to start" | |
| `notready.set_aside` | Set aside by a label | `kind` `put_aside` | Tab "Issues to start" | |
| `notready.no_goals` | No goals written for this project | `kind` `no_goal_ledger` | Tab "Issues to start" | |
| `notready.goals_error` | The project's goals cannot be read | `kind` `goal_ledger_error` | Tab "Issues to start" | |
| `notready.no_repo` | No repository recorded for this project | `kind` `no_repository` | Tab "Issues to start" | |
| `tab.failures` | Failures | Every row of "Failures" | Shelf | |
| `tab.approved_items` | All approved items | `ranges[].items[]` of the selected range | Shelf | |
| `tab.issues` | Issues to start | `next_work.candidates[]` and `.left_out[]` | Shelf | |
| `tab.one_off` | One-off tasks | `runs[]` with no range | Shelf | |
| `tab.prs` | Pull requests | Pull requests of runs not closed | Shelf | |
| `tab.history` | Task history | The events of one task | Shelf | |
| `tab.instructions` | Instructions | `ranges[].brief_head` | Shelf | |
| `tab.mark_failed` | N failed | Failed rows behind the view | Sidebar entry, second line | |
| `tab.mark_needs_you` | N need you | Rows behind the view that wait for the person | Sidebar entry, second line | |
| `answer.title` | Needs your answer | An escalated entry of `pending[]` | Answer card, "Failures" | |
| `answer.kind_decision` | A decision | A stop of kind `design`, `scope`, `order`, `naming`, `other` | Answer card heading | |
| `answer.kind_question` | A question | An entry outside a range | Answer card heading | |
| `answer.kind_permission` | A permission request | An entry of kind `prompt` | Answer card heading | |
| `answer.background` | Show the background | `pending[].context.background` | Answer card | |
| `answer.if_yes` | If yes | `context.if_yes` | Answer card | |
| `answer.if_no` | If no | `context.if_no` | Answer card | |
| `answer.recommended` | recommended | `context.recommend`, `recommend_answer` | Answer card | |
| `answer.why` | Why | `context.why` | Answer card | |
| `answer.yes` | Answer yes | `answer <id> yes` | Answer card | |
| `answer.no` | Answer no | `answer <id> no` | Answer card | |
| `answer.discuss` | Discuss it first | Moves to the conversation; sends nothing | Answer card | |
| `fail.title` | Failures | The region "what went wrong" | Right column | |
| `fail.did_not_start` | did not start | A run `failed` whose newest launch is `failed` | "Failures", "Active now" | |
| `fail.silent` | silent since it started | Anomaly `no_status` | "Failures" | |
| `fail.gone` | its terminal is gone | Anomaly `gone` | "Failures" | |
| `fail.error` | reports an error | Anomaly `error` | "Failures" | |
| `fail.stuck_question` | stopped at a question | Anomaly `blocked_question` | "Failures" | |
| `fail.stuck_login` | stopped at a login | Anomaly `blocked_auth` | "Failures" | |
| `fail.no_report` | finished without a report | Anomaly `done_no_report` | "Failures" | |
| `fail.secretary_gone` | The secretary's terminal is gone | Anomaly `secretary_gone` | "Failures" | |
| `fail.secretary_still` | The secretary has not moved | Anomaly `secretary_still` | "Failures" | |
| `who.nobody` | Nobody is on it | 4.1, "nobody is acting" | "Failures" | |
| `who.no_sign` | No sign that anyone is on it | 4.1, no record of handling | "Failures" | |
| `who.secretary` | With the secretary | 4.1 | "Failures" | |
| `who.told` | told N min ago | The row's notice is delivered | "Failures" | |
| `who.looking` | looking | An act of the secretary on the task since | "Failures" | |
| `who.you` | Needs your answer | An escalated entry of the row's task | "Failures" | |
| `act.ask_look` | Ask the secretary to look | `send secretary` with the fixed sentence of the kind and the task id | "Failures" | |
| `act.history` | Show its history | Opens the tab "Task history" on that task | Any row naming a task | |
| `act.abandon` | Abandon and close… | `close <task> --abandon`, behind a confirmation | "Failures" | |
| `act.show_question` | Show the question | Shows that entry in the answer card | "Failures" | |
| `act.in_shell` | In your shell: | The line to type | Read-only controls | |
| `place.merged` | merged | Place `landed` | Band, lists | |
| `place.closed` | closed, not merged | Place `closed` | Band, lists | |
| `place.needs_you` | needs you | Place `waiting_person` | Band, lists | |
| `place.in_review` | in review | Place `review` | Band, lists | |
| `place.working` | being worked on / working | Place `in_work` | Band, lists | |
| `place.not_started` | not started | Place `not_in` | Band, lists | |
| `place.skipped` | skipped | Place `skipped` | Band, lists | |
| `stage.checks_running` | checks running | Stage `ci` and a watch `watching` of the task in `pr_watches[]`. Stage `ci` alone is not enough: the range's read gives it to every head whose newest watch is not a passed one, a failed CI and an ended watch included (3.5) | Lists, pull requests | |
| `stage.checks_failed` | checks failed | Stage `ci`, no watch `watching`, and the newest `ci_completed` of the task's head `failed` | Lists, pull requests | |
| `pr.closed_outside` | closed outside makiba | `pr_closed` (Issue 388) newer than the task's newest `pr_reopened` | Lists, pull requests, trail | |
| `pr.reopened` | reopened · checks not watched | `pr_reopened` with no `ci_watch_started` of the pull request after it | Lists, pull requests, trail | |
| `pr.merged_gate` | through the gate | `pr_merged.merged_via` `gate` | Pull requests, trail | |
| `pr.merged_hand` | by hand, outside the gate | `pr_merged.merged_via` `hand`, with `merged_by` | Pull requests, trail | |
| `gap.missing` | missing: FIELD | A field the read surface does not give (section 6); drawn as the gap of 2.5, never filled | Any part | |
| `stage.ready_to_merge` | checks passed · ready to merge | Stage `to_land` | Lists | |
| `stage.no_pr` | no pull request yet | Stage `no_pr` | Lists | |
| `stage.free_place` | waits for a free place | `why` `no_slot` | Lists | |
| `stage.budget` | the budget is used up | `why` `budget` | Lists | |
| `stage.waits_for` | waits for #N | `why` `waits_for #N` | Lists | |
| `active.title` | Active now | Items failed to start, waiting, at work or in review | Right column | |
| `active.stuck_first` | stuck ones first | The order "stopped first" | Right column, tab "One-off tasks" | |
| `list.more` | N more | Rows under a cap | Any capped list | |
| `list.close` | close | Closes the open tab | Shelf | |
| `hist.started` | Started by makiba / by the secretary | `belt_dispatched`, `delegate_sent` | Tab "Task history" | |
| `hist.began` | Began working | `worker_spawn_verified` | Tab "Task history" | |
| `hist.plan` | Reported its plan | `worker_reported` (start) | Tab "Task history" | |
| `hist.done` | Reported done | `worker_reported` (done) | Tab "Task history" | |
| `hist.review` | Review passed / found N | The review rounds' events | Tab "Task history" | |
| `hist.asked` | Stopped to ask you | `belt_stopped` | Tab "Task history" | |
| `hist.pr` | Pull request opened / updated | `pr` events | Tab "Task history" | |
| `hist.checks` | Checks passed / failed | `ci_completed` | Tab "Task history" | |
| `hist.merged` | Merged | `pr_merged` | Tab "Task history" | |
| `hist.closed` | Closed / Abandoned | `run_closed` | Tab "Task history" | |

### 2.5 The visual system

The owner asked (Issue 394) for a screen on a level with today's developer tools: the polish of current issue trackers, CI dashboards and deploy consoles. That is taken as a quality bar, not as something to copy: no other product's name, logo, colours or distinctive layout. The system below is small on purpose; every part of the drawing uses it and nothing else. Its values are the custom properties at the top of the drawing's style, and the drawing's last two panels show it whole. Round 2 (the owner's reading of 2026-11-24, section 2) changed its tone: structure comes from whitespace, rules and the weight of the type, not from boxes.

**Principles.** One surface per column, no card inside a card: a list is rows divided by hairlines, a section is a heading and whitespace. No pills and no coloured tags: a state is a mark and a word; a size or a goal clause is a word in the meta line ("G2 · size M, estimated"). Colour is spent where it carries meaning, and red least of all. One region leads (the centre, with the largest type); the sidebar and the right column are quieter and smaller.

**Type.** System fonts only; nothing is fetched when the screen is viewed. Sans: `system-ui, -apple-system, "Segoe UI", "Noto Sans", "Hiragino Sans", "Noto Sans JP", "Liberation Sans", sans-serif` (the Japanese faces are there for the display language of 2.2). Mono, for references, task ids, heads and lines to type: `ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, "DejaVu Sans Mono", "Liberation Mono", monospace`. Numbers are tabular everywhere, so counts and ages do not jitter. The body is 14 px (it was 13): the owner found the text small.

| Step | Size / weight | Used for |
| --- | --- | --- |
| xl | 20 / 650 | The question waiting for the person: the largest text on the screen |
| lg | 16 / 650 | The heading of the view (the selected range, "Pull requests") |
| md | 14 / 400 (500 to 650 for titles) | Body: rows, the sidebar's names, the region headings of the right column |
| sm | 13 / 400 | Meta: ages, reasons, the second line of a row, links and buttons |
| xs | 12 / 600 | Group headings in a list, the sidebar's section names, the state line under a sidebar entry |

Line height 1.5. A title is not cut where the column is wide enough to hold it (the centre); where it is cut, it keeps 16 characters and shows whole on hover.

**Colour.** One token per role, each with a light and a dark value; no part has a colour of its own (2.2). Neutrals: `bg` (the sidebar, the top line, the folded conversation), `surface` (the centre and the right column), `surface-2`, `border`, `border-strong`, `text`, `text-2`, `text-3`, and `accent` (links and focus only). Four state families, each with a word colour and a mark colour: `ok` (green: work that moves or landed), `wait` (amber: waits for the person, nothing else), `fail` (red: went wrong, nothing else), `info` (blue: in review and pull requests in flight); `idle-mark` (grey) for not started, skipped, closed, and a failure that probably ended. The state tints and row tints of round 1 are kept as tokens (the stale band uses the red solid) but no longer fill badges or rows. The primary button is ink (`primary`, the text colour), so that green means only "moves or landed".

| Token | Light | Dark |
| --- | --- | --- |
| `bg` / `surface` / `surface-2` | `#f4f5f7` / `#ffffff` / `#f0f2f5` | `#0e1013` / `#15181d` / `#1b1f26` |
| `text` / `text-2` / `text-3` | `#16191d` / `#434a55` / `#5d6572` | `#e7eaef` / `#b8bfca` / `#959eac` |
| `accent` | `#2754c5` | `#86a6ff` |
| `ok` word / mark | `#18703a` / `#24944b` | `#5fca83` / `#4fb873` |
| `wait` word / mark | `#8a4b00` / `#b26a00` | `#f1b650` / `#e0a33a` |
| `fail` word / mark | `#b3241b` / `#d0291d` | `#ff887c` / `#f2695d` |
| `info` word / mark | `#2c52a3` / `#3b66c4` | `#95b3ff` / `#7f9ff0` |

Contrast was computed for every pair the drawing uses, from the drawing's own values (the WCAG 2 relative-luminance formula): each text colour, state word and link on `bg`, `surface` and `surface-2`; white or ink on each solid and on the primary button; the gap's text; and the pairs round 1 used, which are kept as tokens. Every text pair passes AA (4.5:1); the lowest is `text-3` on `surface-2`, 5.25 in light and 6.11 in dark. Marks and the focus ring are non-text and need 3:1; the lowest is the green mark, 3.88 on `surface` (light). No text is faded with opacity: a greyed region takes `text-3`.

**Red by degree.** Red is for what went wrong, and within that, for what is stopped with nobody on it. A failure row has one of three degrees, taken from its kind and from who is on it (4.1): *stopped, nobody on it* (did not start, terminal gone, error, silent, with no sign of handling): the red mark, the title in bold, and one red line "No sign that anyone is on it"; *waits for you* (an escalated question of its task): the amber mark and one amber line; *probably ended or handled* (finished without a report, or with the secretary): the grey mark, the title in regular weight, the handling in grey. The kind and who is on it are one sentence in one line, the only coloured line of the row ("Did not start · no sign that anyone is on it"). Rows are ordered by degree, then newest first. The right column shows the first degree only, at most three rows, and a fold line that names what the others are. Nothing else on the screen is red: not the card, not the border, not the row's background, not the column's heading; the sidebar says "3 unattended" in red words, and the top line colours only the figure.

**State is never colour alone.** Each place has a mark of its own shape, a word, and a colour: merged (a filled circle with a check, green), closed, not merged (a square outline, grey), needs you (a filled circle with a question mark, amber), in review (a filled diamond, blue), being worked on (a ring with a centre, green), not started (a dashed ring, grey), skipped (a ring struck through, grey), did not start or failed (a triangle with an exclamation mark: red when stopped with nobody on it, grey when it probably ended). Two pull-request states have their own: reopened (a diamond outline, blue) and merged by hand (a diamond outline with an exclamation mark, amber). The shapes differ in silhouette, so they read in grey too and with any colour vision. The marks are inline SVG; none is an emoji or a font glyph.

**Spacing and density.** A 4 px grid: 4, 8, 12, 16, 24, 32, 40. The centre is padded 32 at the sides and leads; the right column 24; the sidebar 12. Sections are separated by 24 to 32 of whitespace and, between the answer and the view, by one hairline. Controls are 32 px high (28 when small) with a 6 px radius. A row is 40 px for one line and grows by a line for its explanation. Action links in a row are text, not buttons; only the answer, "Ask to start" and the approval have buttons.

**References.** `project#N` everywhere (section 2, round 3): mono 13 px, the project's slug in ink and semibold, `#N` in `text-3`, so the repository is read first and down a column. Never `#N` alone; `OWNER/NAME` only in a repository's heading and on hover.

**Rows.** A row is a grid: the mark (18 px), the reference (`project#N`, mono, in a column of 104 px so that the projects line up), the title with its explanation on a second line in `text-3`, and the state with its age at the right (never cut; the state word takes the state's colour only for "needs you" and "did not start"). Rows are divided by hairlines, never boxed; a group of rows has a 12 px heading with its count. A capped list ends with a fold line: the link names how many more, the words after it say what they are.

**Missing fields.** A field the read surface does not give is drawn as a gap: a dashed, hatched box in mono that names the field ("missing: runs[].title"), never an invented value and never an empty space. A null the record holds on purpose is not a gap: it is said in words ("The secretary wrote no if yes"). The conversation, which the read surface leaves out by design, is folded to its one line (section 2) instead of being drawn as an empty box.

**Empty, loading, error and stale.** Every region has the four, in the same form (the drawing's panel of the states):

- Empty: a sentence that says why it is empty and what will change it ("No Issue is ready to start. The check of 09:41 found none. The next check runs after a task closes."), with the not-started mark. Never a blank area.
- Loading: the region's heading with "Reading…" and grey bars the shape of its rows, so the layout does not move when the rows arrive. No spinner alone.
- Error: a red rule at the left of the region's top, with the error as recorded, whole, and what is still shown under it (the last good list with its age).
- Stale: when the heartbeat is old, a red band across the top of the screen ("makiba has not responded for 4 min", with the line `makiba up`) and every region greyed: its marks lose their colour and its text takes `text-3`, so it stays readable at AA (2, rules of the look).

**Focus and keys.** Every control is reachable by the keyboard. The focus ring is 2 px of `accent` with a 2 px gap, in both themes, and is never removed. Tab moves within a region in reading order; F6 moves to the next region, in the order of what calls for the person first: the top line, the answer, the view, "Failures", "Ready to start", the sidebar, the conversation line (Shift+F6 goes back). The two counts of the top line are links to their regions. In the sidebar the arrow keys move between entries and Enter selects one; Escape in a view goes back to the selected range. No single-letter shortcut: the conversation, once open, is a terminal that takes every key. Each region is a landmark with its name ("Needs your answer", "Failures", "Approved work 3"), so a screen reader can jump to it.

**Widths.** 1440 px is the size the screen is designed at (248, the rest, 360); the top line never widens the grid, it gives way in the order of 2.1. The narrow width is 1024 px (the drawing's third picture, 1024 × 720): the sidebar (216) and the centre; the right column's two regions are views of the sidebar there (section 2). Below 1024 px nothing is designed (section 8).

## 3. Each part and its read field

The screen opens no table and no file. It reads `makiba status --json` on an interval and `makiba events --json --after <id>` for the trail (spec section 12). "Today" means the field is on the read surface now; "missing" means the part cannot be drawn until the field exists, and section 6 lists those.

The tables below are by role; where a part is drawn is section 2. "Ready to start" feeds the first candidates and the tab "Issues to start", which also holds "Not ready yet"; the range's rows feed "Active now" and the tab "All approved items"; "Runs outside a range" is the tab One-off tasks; "The trail of one run" is the tab Task history; the brief head is in the approval and in the tab "Instructions". A tab's count and colour are computed from the same rows as its content, never kept apart from it.

### 3.1 Put in

| Part | Read field | State |
| --- | --- | --- |
| The range: id, project, status, terms | `ranges[].id`, `.project`, `.status`, `.issues`, `.takes`, `.at_once`, `.approval_mode` | Today |
| Budget used | `ranges[].budget.tasks`, `.used.tasks` | Today (hours and fixes are belt step 2) |
| Approved when | `ranges[].approved_at` | Today |
| A proposed range and its read | `ranges[].candidates[]` (`issue`, `title`, `admitted`, `left_out_why`), `.read_id`, `.read_at`, `.read_complete` | Today |
| What the approval lets in | `ranges[].issues` (every number, never "and N more") and `ranges[].takes` (the condition as written), both whole, over the read and before the button: the read says what the range admits now, the list and the condition say what it may admit later. With `at_once`, `budget.tasks` and `approval_mode` | Today |
| What every task of the range starts from | `ranges[].brief_head`, whole, never cut: the approval covers it (belt.md section 1, "Approve"), so a proposed range shows it in full, scrolling inside its box, before the button can be pressed | Today |
| Could go in now | `next_work.candidates[]`: `repo`, `issue`, `title`, `clause_id`, `size`, `size_estimated`, `why`, `recommended` | Today |
| Cannot go in yet | `next_work.left_out[]` with an `issue`: `repo`, `issue`, `kind`, `reason`; `candidates_not_recorded` and `left_out_not_recorded`, both numbers, shown as "and N more not listed" | Today (a left-out entry has no title: missing) |
| Projects the scan could not rank | `next_work.left_out[]` with a null `issue`: `project`, `repo`, `kind`, `reason`. A whole repository (`no_goal_ledger`, `goal_ledger_error`) is named by its project and repository; `no_repository` has a null `repo` too and is named by its project alone; an entry with no project is named by its repository. They are listed under their own heading in the tab "Issues to start", "Projects not ranked", after the Issues, and are not counted as held Issues: the tab's heading reads "14 · not ready 5" for Issues and adds "· 1 project unchecked" | Today |
| The scan read only part | `next_work.input_truncated`, a list of the reads cut short (`open_issues`, `open_prs`, `base_merges`), not a count: shown as its own warning, "the scan did not read every open Issue", naming the reads | Today |
| Why the list is empty | `next_work` null (no scan yet), `.status` (`no_candidates`, `error`), `.error`, `.scanned_at`, `.trigger` | Today |
| A scan recorded before the lists were kept | `next_work.candidates` and `.left_out` null while `.candidate_count` is above 0 (a scan older than Issue 256): shown as "N found; their details were not recorded, the next scan will list them", never as "nothing could go in" | Today |

While a range is active the list is headed "outside the range" (belt.md 7.1). An Issue the range would admit is shown once, in the range. The match is by repository and number, never by number alone: a range's repository is `projects[].repo` of its `ranges[].project`, and a candidate carries `next_work.candidates[].repo`. `projects[].repo` is null when the repository is not recorded, though the belt works such a project from its clone's origin: then the match is by the project's slug (`next_work.left_out[].project`; a candidate of that repository is matched once `ranges[].repo` exists, section 6) and the reference is written `<project>#N`. With more than one project registered every reference on the screen is written `OWNER/NAME#N`; with one, `#N` (as the drawing).

One point where the owner's words and the record differ: the owner lists "matching no goal clause" among the reasons an Issue cannot go in. The scan does not leave such an Issue out: it is a candidate with `clause_id` null, ranked after the ones that name a clause. The drawing follows the record (a candidate tagged "no clause", ranked last). If the owner wants it under "cannot go in yet", that is a change to the scan, not to the screen.

### 3.2 Answer

| Part | Read field | State |
| --- | --- | --- |
| The entry: number, task, age, question | `pending[].id`, `.task_id`, `.created_at`, `.question`, `.status` | Today |
| The card's one-line heading | The kind of entry and the task's title: for a range's stop, `ranges[].stops[].kind` ("A decision on design", "on scope", ...) and `ranges[].items[].title` by the entry's `task_id`; for another question, "A question" and the task id; for a permission prompt, "A permission prompt" and the task id. Never a cut copy of the question | Today (a title for a task outside a range is missing, section 6: `runs[].title`) |
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
| "Did not start" inside the count of those being worked on | Not a place and not in `places`. An item counts as "did not start" by the same cross-check as a row of "Failures" (the row above on a failed launch): `place` `in_work`, the run of its `task_id` in `runs[]` has `status` `failed`, **and** that run's `stage` (its newest launch) is `failed`. The item's own `stage` `launch_failed` is not enough: the range's read gives it to every failed run, a worker that was verified and whose terminal later disappeared included. Such an item is shown in the band and in "Active now" by its anomaly's words ("its terminal is gone"), red, and is not counted as "did not start". Shown as "3 being worked on (1 did not start)", the red part of that piece of the strip and a red `!` on the row; the same run is in "Failures" | Today |
| Skipped, and why | `ranges[].skipped[]` | Today |
| An ended range | `ranges[].status`, `.ended_reason`; its list stays while it is the project's newest (belt.md 7.1) | Today |
| Failures: an open anomaly | `anomalies[]`: `task_id`, `rule`, `since` | Today |
| Failures: a failed launch | A run whose `runs[].status` is `failed` **and** whose `runs[].stage` (its newest launch) is `failed`: the launch never reached `verified`. Neither alone says it. The status alone does not: the anomaly `gone` also sets a run `failed` when a worker that had launched well loses its terminal; its newest launch is then `verified`, and the run is shown once, as that anomaly, never also as "launch failed". The stage alone does not: `makiba down` sets a launch's stage to `failed` while it suspends the run, and writes no `worker_spawn_failed`; a run `suspended` is shown as "suspended" under the runs, not as gone wrong. The reason is shown only from a `worker_spawn_failed` event whose `spawn_id` is the run's newest launch; an older launch's failure is never attached | The status and the stage today. The reason is derivable from the events (3.5): `worker_spawn_failed` carries the task (through its run) and the `spawn_id`, and the newest launch's `spawn_id` is that of the task's newest `run_taken` or `worker_spawned`. A launch that wrote neither cannot be tied, and the row then says "launch failed" with no reason; `runs[].launch_failed_reason` (section 6) would make it sure |
| Failures: the dispatcher is silent | `makibad.running`, `.heartbeat_at` | Today |
| Failures: the last scan failed | `next_work.status` `error`, `.error` | Today |
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

### 3.5 The inventory: every part against the read surface of 2026-11-24

Read against the code at the base of Issue 394 (`src/status_local.rs`, `src/daemon/belt.rs` `read_ranges`, `src/daemon/triage.rs`, `src/daemon/prwatch.rs`) and spec sections 10 and 12. **Given**: a field prints it as the part shows it. **Derivable**: the screen computes it from given fields, by the rule written. **Missing**: neither output carries it; the letter names the addition proposed in section 6 (each its own Issue), or the Issue that already holds it. The fixture of [screen-fixture/](screen-fixture/) has every given field, and the drawing shows every missing one as a gap.

The fixture: `status.json` is one object of `makiba status --json` and `events.json` one page of `makiba events --json`, with the field names and the nesting the code prints, in its order. Every value is invented: the projects `loom` (repository `example-org/loom`) and `atlas` (no repository recorded), the account `example-maintainer`, the titles, the heads and the times. It was written from the code and the spec, not exported from a running organization: the sandbox of this task cannot open a `state.db`. It is read as at 2026-11-24T10:00:00Z; every age in the drawing is counted from that time. What it holds (round 3): four projects, `atlas` (no repository recorded), `harbor`, `loom` and `quill` (`example-org/...`); range 3 of `loom` active with twelve items in all seven places (a launch failed, a merge through the gate, a merge by hand, a pull request closed and reopened outside makiba, one under watch, one passed), range 5 of `quill` and range 6 of `harbor` active, range 7 of `harbor` proposed to replace range 6 (a launch failed and an error in harbor, a question in quill), range 4 of `atlas` proposed with its read, range 2 of `loom` ended and replaced with one run left whose terminal is gone; three one-off runs, four pending entries (three escalated, one permission prompt still with the secretary), five open anomalies (one of the whole organization, with no task: `binary_replaced`), one watch, a scan with nine candidates over three repositories and five left out, recorded whole in its event as `status_local::next_work` reads it back, and seventy-two events (the notice written after the merge by hand among them), among them the whole trail of one task. Numbers collide across repositories on purpose: `loom#56` and `quill#56`, and the pull requests `loom#127` and `harbor#127`.

**Top line**

| Part | Field | State |
| --- | --- | --- |
| The organization's name | none | Missing (E). Drawn as a gap |
| "Needs you N" | `pending[]` with `status` `escalated` | Given (a count) |
| "Failed N" | `anomalies[]`, plus each run with `status` `failed` and `stage` `failed`, plus `next_work.status` `error`, plus `makibad.running` false | Derivable (3.3: a run `failed` whose newest launch is `verified` is its anomaly, counted once) |
| "unattended N" | The handling state of 4.1 | Derivable only in part: whether a notice was delivered and whether the secretary looked are missing (D), so every row with no other sign counts here, 3 of 4 in the fixture (question 6) |
| "Running · updated N s ago" | `makibad.running`, `.heartbeat_at`; the age of the screen's own read | Given |

**Hand over work**

| Part | Field | State |
| --- | --- | --- |
| The selected range: id, project, status, terms | `ranges[].id`, `.project`, `.status`, `.issues`, `.takes`, `.at_once`, `.budget.fixes` | Given |
| Tasks and hours used | `ranges[].used.tasks` of `.budget.tasks`; `.used.hours` of `.budget.hours` (null: no hour budget, the meter is not drawn) | Given |
| Approved when, by whom, merges approved by whom, new Issues let in or asked | `ranges[].approved_at`, `.approved_by`, `.approval_mode`, `.follow_ups` | Given |
| Which range is selected | `ranges[].status`, `.places`, the runs of its items | Derivable (2.1) |
| The other ranges' lines | `ranges[]` other than the selected one: id, project, status, `ended_reason`; "1 task left" is its items with a `task_id` whose run is in `runs[]` | Derivable |
| A proposal: what it may let in, the read, the brief | `ranges[].issues`, `.takes`, `.candidates[]` (`issue`, `title`, `admitted`, `left_out_why`, `design_only_waits`), `.read_id`, `.read_at`, `.read_complete`, `.brief_head` | Given |
| A reference of a project with no repository | `projects[].repo` null: `<project>#N` | Derivable; the repository the belt resolved is missing (H) |
| Ready to start | `next_work.candidates[]`: `repo`, `issue`, `title`, `clause_id`, `clause_heading`, `size`, `size_estimated`, `why`, `recommended`; `.scanned_at` | Given |
| Ready, minus what a range already holds | A candidate whose `repo` and `issue` match an item of an active or proposed range is shown there only (#60 in the fixture) | Derivable (3.1) |
| Not ready yet: the reason | `next_work.left_out[]` with an `issue`: `kind`, `reason` | Given |
| Not ready yet: the title | none | Missing (B). Drawn as a gap |
| Projects not ranked | `next_work.left_out[]` with a null `issue` | Given |
| Why the list is empty, the scan cut short | `next_work` null, `.status`, `.error`, `.input_truncated`, `.candidates_not_recorded`, `.left_out_not_recorded` | Given |

**Answer**

| Part | Field | State |
| --- | --- | --- |
| The entry: id, task, age, question whole | `pending[].id`, `.task_id`, `.created_at`, `.question` (only its first line for a permission prompt) | Given |
| The kind in the heading | `ranges[].stops[].kind` by the entry's id ("A decision on design"); "A question" otherwise; `pending[].kind` `prompt` | Given |
| The title in the heading | A range's task: `ranges[].items[].title` by `task_id`. A one-off task: none | Given for a range; missing for a one-off task (A), drawn as a gap |
| Background, if yes, if no, recommendation, why | `pending[].context.*`; a null part is said in words, it is not a gap | Given |
| Which answer is recommended | `ranges[].stops[].recommend_answer` | Given for a range's stop; for another entry only `context.recommend`, as text |
| "1 more question is with the secretary" | `pending[]` with `status` `open` | Given |

**The conversation**

| Part | Field | State |
| --- | --- | --- |
| The bar: state and age | `terminals[]` with `role` `secretary`: `state`, `since` | Given |
| What is said | none, by design | Not on the read surface; in herdr-app the terminal itself (3.4). Drawn as one gap |

**Progress**

| Part | Field | State |
| --- | --- | --- |
| A failure: an anomaly | `anomalies[]`: `task_id`, `rule`, `since` | Given |
| A failure: a launch that failed | `runs[].status` `failed` and `.stage` `failed` | Given |
| Its reason | The newest `worker_spawn_failed` of the task whose `spawn_id` is that of the task's newest `run_taken` or `worker_spawned` | Derivable from the events ("no first turn within 120 s" in the fixture); sure only with G |
| Its title | A range's task: `ranges[].items[].title`. A one-off task: none | Missing for a one-off task (A) |
| Who is on it: needs your answer | An escalated `pending[]` of the row's task | Given |
| Who is on it: nobody (the two anomalies of the secretary) | `anomalies[]` `secretary_gone`, `secretary_still` | Given |
| Who is on it: with the secretary, told N min ago | The delivery of the row's notice | Missing (D) |
| Who is on it: looking | The secretary's last act on the task | Missing (D); today only from the secretary's events with that task id |
| The band and its counts | `ranges[].places`, `.items[].place` | Given |
| "(1 did not start)" in the band | An item `in_work` whose run is `failed` with `stage` `failed` | Derivable (3.3) |
| Active now: place, stage, reason | `ranges[].items[]`: `place`, `stage`, `why`, `task_id`, `issue`, `title` | Given |
| Active now: how long in review | `items[].since` of a review item is its dispatch, not when it went into review | Missing (J); the drawing shows the age of the newest pull-request event instead, from the events |
| "checks running", "checks failed", "reopened · checks not watched" | Stage `ci` covers all three: the read gives `ci` to every head whose newest watch has not passed (running, failed, or ended because the pull request closed). Told apart by `pr_watches[]` (watching), the newest `ci_completed` (failed) and `pr_closed` / `pr_reopened` / `ci_watch_failed` (closed, reopened) | Derivable from the events; a field per pull request is missing (C). The wording of 2.4 was corrected: `ci` alone is not "checks running" |
| "checks passed · ready to merge" | Stage `to_land` | Given |
| Merged, through the gate or by hand | `pr_merged`: `merged_via` (`gate` or `hand`), `merged_by`, `head_oid`, `merge_commit` (Issue 388); a merge recorded before Issue 388 has none of the first three | Given, in the events only; on `status` only `items[].pr` and `.pr_title` |
| A pull request closed outside makiba, reopened | `pr_closed` (`repo`, `pr`, `title`, `task_id`), `pr_reopened` (`repo`, `pr`, `task_id`) (Issue 388) | Given, in the events only |
| The secretary was told of a hand merge, a close, a reopen | A `message_sent` of `makibad` with the task, right after; the body is not on the read surface | Derivable that a notice was written; whether it was delivered is missing (D) |
| The merge state: mergeable, behind, in conflict | none (Issue 387 reads `mergeable` and `mergeStateStatus` in the same state read; not yet merged) | Missing (Issue 387, then C). Drawn as a gap on each open pull request |

**Several repositories (round 3)**

| Part | Field | State |
| --- | --- | --- |
| The project of a row | A range's item: `ranges[].project`. A run: `runs[].project`. A failure: the project of its run (`anomalies[].task_id`, or the failed run). A question: `pending[].task_id` to `runs[].project`. A candidate or a left-out Issue: `next_work.candidates[].repo` to the project whose `projects[].repo` it is (`left_out[]` also carries `project`). A pull request: `repo` of its event, to its project | Derivable for every row; given for ranges, runs and left-out entries |
| `project#N` | The slug (`projects[].slug`) and the number | Derivable; the slug is unique in the organization |
| The per-repository counts of the top line and the sidebar | The same rows as the totals, grouped by their project | Derivable |
| A failure of the whole organization | A row with no project: an anomaly with a null `task_id` (`binary_replaced`), `secretary_gone`, `secretary_still`, a silent `makibad` (`makibad.running`), a failed scan (`next_work.status` `error`, which spans every repository) | Given; it is counted as "organization" in the top line, comes first in "Failures", and is shown under every filter, never hidden by one |
| A row of a closed run (a pull request merged today) | `repo` of `pr_merged` to its project; the run itself is no longer in `runs[]` | Derivable from the events |

**Behind the tabs**

| Part | Field | State |
| --- | --- | --- |
| All approved items | `ranges[].items[]` in its order | Given |
| One-off tasks: id, kind, project, status, the terminal's state and age | `runs[]` with no range item naming its `task_id`; `terminals[]` by `w-<task_id>` | Derivable (no `range_id` on a run: a run is a range's when an item of a range names it) |
| One-off tasks: title, start, Issue, range | none | Missing (A). Drawn as a gap |
| One-off tasks: waiting for you, woken | `runs[].waiting`, `.unparked` | Given |
| Pull requests: the open ones of runs not closed | The `pr_opened` and `fix_pushed` of tasks in `runs[]`, with what the events say of each head (above) | Derivable from the events; missing as a field (C) |
| Pull requests: the title | Only `pr_merged` and `pr_closed` carry one; for a range's task the item's title stands in | Missing for an open pull request of a one-off task (C). Drawn as a gap |
| Pull requests: merged today | `pr_merged` since local midnight | Derivable from the events |
| Task history | Every event with that `task_id`, oldest first; the review rounds (`codex_round_started`), the report (`worker_reported`, written only when a review answered), the gate (`acceptance_gate`) | Given in the events; a filter by task is missing (I) |
| Instructions | `ranges[].brief_head` | Given |

What the inventory changed in this note: the launch-failure reason moves from missing to derivable (3.3); the wording `stage.checks_running` was wrong for stage `ci` and is corrected (2.4); a review item's age is not its time in review (J); the organization's name has no field (E); the merge state is Issue 387's.

## 4. Acting from the screen

Reading needs no key. Every act is a verb of the door, sent as the person's `makiba` sends it; the screen adds no verb and no way around one.

| Control | Verb | Key | Note |
| --- | --- | --- | --- |
| Answer yes / Answer no, with a note | `answer <pending id> yes\|no [--note]` | Person | `stale` or `conflict` is shown in the entry; nothing is retried. |
| Discuss it first | none | none | Moves the focus to the conversation. |
| Approve exactly this | The door's `range` `approve` with `read` set to the `ranges[].read_id` the screen showed | Person | A newer read is `stale`: the screen shows the new read and asks again. The person's command cannot say which read: `makiba range approve <id>` fetches the newest read itself, so with `--yes` it could approve a read the screen never showed. The screen therefore sends the door request, or the command gets a `--read <id>` argument first (part of the boundary design below). The read-only screen's line to type is `makiba range approve <id>` without `--yes`: the command prints its read and asks. |
| Hold new tasks | `range pause <id> --reason` | Person | The reason is asked for; "paused from the screen" when left empty. |
| Start new tasks again | `range resume <id>` | Person | |
| End after running tasks | `range end <id>` | Person | Asks once: work already running goes on to its end. |
| Ask for this | `send secretary <text>` | Person | A fixed sentence naming the repository and the number only: "Please take up OWNER/NAME#N." No title: the secretary reads the Issue. The secretary decides how; the screen dispatches nothing. |

"Ask to start" is a message and not a `dispatch` so that the secretary stays the one contact (goal G1) and the brief stays the secretary's work. A new range is drafted in the conversation (belt.md section 1); the screen has no form for one.

This is a trust boundary, and it is designed separately before any of it is built: a program other than the person's shell would send requests with the person key. The questions that design must answer: who holds the key (the screen's own process reading `person.key` as `makiba` does, or the screen running the `makiba` binary); how a page's requests are told from another local program's (a page on 127.0.0.1 can be asked by anything on the machine, an agent included, unless its sandbox has no route there); and that a click is recorded as the person's act through the screen (`commands.actor`). Until that design is agreed, the screen is read-only and each control shows the line to type instead (`makiba answer 14 yes`), which the person runs in their shell.

A line to type is an output that ends in the person's shell, so it never carries record text: no title, no question, no reason, nothing that came from GitHub or from an agent. It is built from the verb, numbers (a pending id, a range id, an Issue number) and the repository as `OWNER/NAME`, taken from `projects[].repo` and shown only when it matches the characters GitHub allows in an owner and a name (letters, digits, `-`, `_`, `.`); a repository that does not match gets no line, and the control says so. Any value that must appear is one single-quoted shell word, with a single quote inside it written as `'\''`. The note of an answer is typed by the person in their shell, not carried by the line. The same rule holds for a door request once the screen acts: the fixed sentence of "Ask to start" carries the repository and the number, never the title.

### 4.1 "Failures": who is handling it, and what can be done

A row that only shows leaves the person asking whether anyone is on it (the owner, 2026-11-23). Each row says who is handling it, and carries its actions in place.

**Who is handling it**, the first that holds:

| State | Shown as | Read from | State of the field |
| --- | --- | --- | --- |
| Waiting for your decision | Amber, "Needs your answer", and the action "Show the question", which shows that entry in the answer card. | An `escalated` entry of `pending[]` with the row's `task_id`. | Today |
| Nobody is acting | A filled red mark, "Nobody is on it": the only filled mark in the list, so it cannot be taken for the others. The top line counts these apart ("Failed 6 · unattended 2"), and the fold line of a capped list says how many of the hidden rows are in this state. | Any of: the row is one the secretary cannot handle (`makibad` silent); `secretary_gone` or `secretary_still` is open in `anomalies[]` (spec section 11: the secretary's status has not changed while a worker's anomaly is open); or there is no act by the secretary on the task since the row began (the sign "looking" of the next state) and the notice of the row was not delivered. An act by the secretary since the row began always wins over an undelivered notice: the row is then "With the secretary". | The two anomalies today. Whether the notice was delivered: missing (section 6). |
| With the secretary | Plain outline, "With the secretary", then what is known: "told N min ago", and "looking" when the secretary has acted on the task since. | Told: the row's notice to the secretary is `delivered` (spec section 11, "Told to"; the delivery ledger). Looking: an act by the secretary on the row's task after the row's `since`. The events give this only in part: an event whose actor is `secretary` and whose `task_id` is the row's (a message sent to the worker, a close). They do not give it for the commonest act: a `pending add` or `escalate` by the secretary leaves `command_received` with no task id, and the `belt_stopped` of an escalation is written by `makibad`. An entry of `pending[]` is no sign by itself either: `makibad` makes one for a permission prompt with no act of the secretary. | "Told": missing (section 6). "Looking": today only from the secretary's task-linked events; for the rest it needs a record of the secretary's last act on a task (section 6). Until both exist, a row with no readable sign, a secretary working through a pending question included, is shown as "No sign that anyone is on it", in the red of "Nobody is on it": the wording says that the screen sees no sign, not that nobody acts. |

**What can be done**, per kind of row. Each control is a verb of the door or a move inside the screen; in the read-only version each verb is shown as the line to type, built by the rule above (the verb, numbers, a task id and a kind; never record text).

| Row | Controls | Verb or line |
| --- | --- | --- |
| Every kind | "Ask the secretary to look" | `send secretary` with a fixed sentence that carries only the kind and the task id: "Please look at <kind> on task <task id>." The kind is one of the screen's own fixed words (`launch_failed`, the anomaly's rule name from spec section 11, `scan_failed`), never text from a record. The task id is shown only when it matches letters, digits, `.`, `_` and `-`; otherwise the sentence names no task and the control says so. As a line: `makiba send secretary 'Please look at launch_failed on task r2-301.'` |
| `makibad` silent | The line `makiba up`, always as a line to type, also once the screen acts: `up` runs from the person's login shell, never over the door (spec section 12). | `makiba up` |
| A failed launch | "Ask the secretary to look"; "Show its history" (the tab Task history, on that task); "Abandon and close…". | "Abandon and close…" is `makiba close <task id> --abandon` behind a confirmation that says what it does: the run is abandoned, its work is not landed, and who abandoned it is recorded. In the read-only version the confirmation is the line with that sentence over it. |
| Relaunching | No control. Nothing retries a failed run today: `makiba up` brings back suspended runs only, never a failed one. | The screen asks the secretary ("Ask the secretary to look" with the kind `launch_failed`); it never dispatches. |
| An anomaly of a worker | "Ask the secretary to look"; "Show its history". | As above. |
| A failed scan | "Ask the secretary to look" with the kind `scan_failed` and no task. | As above. |
| A row waiting for your decision | "Show the question". | A move inside the screen. |

After "Ask the secretary to look" the row says "asked by you N min ago" from the screen's own memory of the send (the read surface shows no message body, so a `message_sent` cannot be tied to a row); the control stays, so the person may ask again. Each control of a row is a button of one line; a row with its mark and its buttons is three lines, and the cap of three rows (2.1) holds.

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

Each is a small addition to `status --json` (or `events`) with a row in the table `Status field` and in `tests/read_surface.rs`; each would be its own Issue, before or with the code of the screen. None blocks the drawing, which draws each as a gap. The letters are those of the inventory (3.5); brought up to date on 2026-11-24 (Issue 394): E, J and the merge state are new, G is now a convenience (the reason is derivable), and C takes in what Issue 388 added to the events.

| | Field | For | Without it |
| --- | --- | --- | --- |
| A | `runs[].title`, `runs[].since`, `runs[].issue`, `runs[].range_id` | "One-off tasks", the heading of a one-off task's question or failure, the trail's heading | A run outside a range is shown by its task id only, with its terminal's age, and its title is a gap. Whether a run is a range's is read from the ranges' items. |
| B | `next_work.left_out[].title` | "Not ready yet" | The row shows the number and the reason; the title is a gap. |
| C | `pull_requests[]`: every pull request of a run not closed, with `repo`, `pr`, `title`, `head_oid`, `task_id`, `ci` (`running`, or the newest conclusion at that head, or `not_watched` after a reopen), `state` (`open`, `closed`, `merged`, with `merged_via` and `merged_by`), and the merge state once Issue 387 reads it | Pull requests; telling "checks running" from "checks failed" and "reopened" in "Active now" | `pr_watches[]` lists only watches under way and `items[].stage` `ci` covers three states, so the screen folds `pr_opened`, `fix_pushed`, `ci_watch_started`, `ci_completed`, `ci_watch_failed`, `pr_closed`, `pr_reopened` and `pr_merged` itself. An open pull request of a one-off task has no title. |
| Issue 387 | The merge state of a pull request (`mergeable`, `mergeStateStatus`: in conflict, behind the base, blocked), read in the same `gh pr view` as Issue 388's state | Pull requests: whether a passed one can be merged as it is | Not shown; a gap on each open pull request. |
| D | `anomalies[].notice` and the same for a failed launch: to whom the row was told and whether that notice is `queued` or `delivered`, with the time; `runs[].secretary_acted_at`: when the secretary last acted on the task through the door, and by which verb (or a `task_id` on `command_received`) | "Failures": who is handling a row (4.1); the top line's "unattended N" | A row with no other sign is "No sign that anyone is on it", and counted as unattended. |
| E | The organization's name (the base's session, or a name the person sets) | The top line | A gap; the screen could name itself after the one project when there is one. |
| F | `conversation_language`, as resolved | Where the display language starts (2.2) | It starts from the environment of the screen's own process; the switch in the top line overrides it. |
| G | `runs[].launch_failed_reason` (the newest launch's `failed_reason`) | Failures | Derived from the events (3.3); a launch that wrote neither `run_taken` nor `worker_spawned` has no reason. |
| H | `ranges[].repo`: the repository the belt resolved for the range | The match of a range's items with `next_work`, and `OWNER/NAME#N` | With a null repository the screen matches by the project's slug and the number, and writes `<project>#N`. |
| I | `events --task <task_id>` | The trail of an old run | The screen keeps what it has read; a run older than its memory is read again from event 0. |
| J | `ranges[].items[].since` of a review item as the time it went into review (today it is the dispatch) | "Active now": how long a task has been in review | The age of the task's newest pull-request event, from the events. |
| - | `pending[].stop_line`, `.to`, and the `belt[]` rows (belt.md section 7, step 5) | The entry's range; what stopped and why, per task | Read from `ranges[].stops[]` and `ranges[].items[]`, which cover a first version. |
| - | `closed_today[]`, or runs closed since a time | "Closed today", "Merged today" | Folded from `run_closed` and `pr_merged`. |

The rule behind the list: the screen states nothing `makibad` could state. Folding events in the screen is accepted for the trail, which is the events, and is a stopgap everywhere else.

## 7. The order of building

Nothing starts before the owner agrees to the drawing.

| Step | What | Needs | Declared test |
| --- | --- | --- | --- |
| 0 | The drawing agreed; the measurement of section 5 run and recorded. | The owner | none (documents) |
| 1 | The model: from `status --json` and `events --json` to the parts of section 3. Read-only. | Step 0; the read surface as it is | `tests/screen_model.rs`: from a recorded status with one escalated entry, one failed launch and a range of mixed places, the model gives the entry with its context, "went wrong" with the failed launch, the range's rows in the read's order, and the runs with the stopped ones first; from a status with `next_work` null, in error, with no candidates, and with a positive count but null lists, four different lines for the empty list; a failed run shown as "launch failed" with no reason (the reason is asserted only once `runs[].launch_failed_reason` exists, section 6) and a suspended run not shown as gone wrong; with two projects, every reference as `OWNER/NAME#N`; a range whose `places` holds a `closed` item and an in-work item of stage `launch_failed`: the counts are those of `places` with `closed` on its own, and the in-work count carries "1 failed to start" without leaving the in-work total; the headings of the tabs (each with its count; red when something behind it failed, else amber when something behind it waits for the person, else plain, with how many); "Active now" as the range's items that are failed to start, waiting, at work or in review, stopped first; every line to type, built from fixtures whose Issue titles, questions and reasons hold a double quote, a single quote, a backtick and `$(touch <a file>)`: no line contains any of that text, and each line, run by `sh` with a `makiba` that only records its arguments, creates no file and passes exactly the verb, the numbers and the repository; a repository name outside the allowed characters gives no line; a replaced range that still has a run not closed: it is offered, selecting it gives its band, its items and its brief, and its run is not among "One-off tasks"; `left_out` entries with a null `issue` (a whole repository, and `no_repository` with a null `repo`): named by project, listed apart, and absent from the count of held Issues; an entry's heading (one line from the kind and the task's title, never from the question) and its question present once; a proposed range whose approval shows every number of `issues` and `takes` whole; five ranges, of which three lines are shown and "2 more ranges"; for "Failures", a row in each handling state: an escalated entry of its task (waiting for your decision), `secretary_still` open (nobody is acting), an event of the secretary on its task after it began (with the secretary, looking), the same with its notice still not delivered (with the secretary all the same), an open entry that `makibad` made for a permission prompt with no event of the secretary after it (no sign that anyone is acting), and none of these (no sign that anyone is acting); a worker verified and then gone: one row, the anomaly `gone`, and no "launch failed" row, and in its range the band counts it among those being worked on with no "did not start" and "Active now" shows it by the anomaly's words; a real sequence in which the secretary adds a pending question on a failing task and escalates it, recorded as `makibad` writes it (a `command_received` with no task id, a `belt_stopped` by `makibad`): the model gives "No sign that anyone is on it" before the escalation and "Needs your answer" after it, and never "With the secretary, looking" from the entry alone; a run `failed` with its newest launch `failed`: one "launch failed" row; six rows gone wrong, one of them a failed scan with no task: three shown, "3 more", and the tab "Failures" holding all six with their controls; the top line's count of rows with nobody acting; the line of "Ask the secretary to look" holding only the fixed kind and the task id, with a task id outside the allowed characters left out; `makiba up` for a silent dispatcher; `close <task> --abandon` only behind its confirmation; no control that dispatches or relaunches; a range of a project whose `repo` is null, referenced as `<project>#N`; every word the model gives to a view found in the wording table of 2.4, and none of makiba's own terms (range, run, place, stage, scan, belt, anomaly, dispatch, launch, brief) in any of them; the top line at four widths (whole; the name dropped; "Display" and "Running · 3 s"; wrapped), the two counts whole in each; the caps of 2.1 (three of six things gone wrong and "3 more", the strip as numbers above twelve items); and every wording read from the one table of 2.2, with a missing key shown by its record name. |
| 2 | The frame chosen in step 0 (the page behind `makiba screen`, or the app's), read-only, each control showing the line to type. | Step 1 | Depends on the frame; for the page, a test that it answers on 127.0.0.1 only and serves what the model gives. |
| 3 | The missing read fields of section 6, each its own Issue. | - | `tests/read_surface.rs` grows. |
| 4 | Acting from the screen (section 4), after its own design and the owner's agreement. | M6-1; the boundary design | Its own. |

## 8. Not in the first version

A notification that calls the person (Issue 272). The organization's own state: capacity, the base, the versions (the owner, 2026-11-22). The screens of workers (the person is never called to a terminal; `peek` stays the secretary's). Several organizations or several people on one screen (belt.md section 10: the fields allow it, the layout does not try). A phone-sized layout.

## 9. Questions for the owner (Issue 394)

Each with a recommendation; the drawing follows the recommendation where it had to choose. The owner's answer (2026-11-24, through the secretary): Q1 to Q9 as recommended, and a redraw before any commit, which became round 2 of the layout (section 2) and of the visual system (2.5). Q4 is answered by that layout (a view opens in the centre); Q7's order of regions follows the new layout (2.5). Q9: the Issues to propose now are A, C and D of section 6. The owner's reading of round 2 (entry 38) withdrew the answer to Q2: with ranges running in several repositories at once, `#N` alone collides; round 3 writes every reference `project#N` (section 2).

| # | Question | Recommendation |
| --- | --- | --- |
| Q1 | Running and landed share green (2.2), told apart by the mark (a ring with a centre, a check); in review is blue. Keep one green for both? | Keep. A second hue for "running" would be a fifth state colour; the marks already part them, in grey too. |
| Q2 (withdrawn, round 3) | With two projects registered, every reference is `OWNER/NAME#N` on a line of its own (2.1). Inside a region tied to one range (its card, its band, "Active now", "All approved items") the drawing writes `#N` and names the project once in the heading. Allow that exception? | Allow it. A range has one project, so `#N` cannot be misread there, and it saves a line on every row. Lists that mix projects (ready, pull requests, failures) keep the long form. |
| Q3 | The narrow width is 1024 × 720: two columns, with "Progress" and "Hand over work" behind a switch whose labels carry their counts (2.5). Agreed as the one narrow layout, with nothing designed below it? | Agree. It is the width of herdr-app beside another window; below it the conversation becomes too narrow to use. |
| Q4 | With a tab open, the left column leaves the tab about two rows at 900 px (the first picture). Should "Ready to start" fold to its one-line count while a tab is open, instead of keeping one row? | Fold it. The tab was opened to be read, and the count with its fold line still says what is there. |
| Q5 | The primary button is ink, not green (2.5). Agreed? | Agree. Green then means only "moves or landed", as 2.2 says of every colour. |
| Q6 | Until the delivery of a notice and the secretary's acts are on the read surface (D), every failure with no other sign counts as "unattended" in the top line: 3 of 4 in the fixture. Count them, or show them apart ("no sign 3")? | Count them as unattended. The wording "No sign that anyone is on it" in the row already says it is the screen that sees no sign; under-alarming is the worse error. Revisit when D lands. |
| Q7 | Keys: Tab within a region, F6 between regions in the order of what calls for the person, arrows in the shelf, Escape to close a tab; no single-letter shortcut (2.5). Agreed? | Agree. The terminal in the centre takes every key, so a letter shortcut would type into it. |
| Q8 | The fixture in `screen-fixture/` becomes the recorded status and events of the model's test in step 1 of section 7. Keep it as that? | Keep it, and grow it there; a change to the read surface then shows up as a change to one fixture. |
| Q9 | Which of the missing fields A to J of section 6 become Issues now, before the code of the screen? | A, C and D now: they are where the drawing shows the most gaps and where the screen would otherwise fold the most events (one-off tasks, pull requests, who is handling a failure). B, E, H and J are small and can go with the code; F, G and I can wait. |

## 10. Feasibility: what today's makiba can give each part of the drawing

The owner's request (2026-11-24): for every element and every button of the drawing, whether makiba gives it today, what is missing, whether and where it can be built, and whether to keep it. Each source was checked in the code at the base of Issue 394: the read surface in `src/status_local.rs` (`read`, `events`), the ranges in `src/daemon/belt.rs` (`read_ranges`, `range`), the scan in `src/daemon/triage.rs` (`shown`), the pull requests in `src/daemon/prwatch.rs`, the verbs and roles in `src/spec.rs` (`Role::verbs`: the person may use every verb) and `src/daemon/door.rs`, the person's command in `src/cli.rs`. "Today" means the field is printed or the verb accepts the person's key now; "derived" means the screen computes it from fields printed today (3.5). The letters A to J are those of section 6.

One condition holds for every button: **today the screen can only show the line to type**, since no program but the person's `makiba` holds the person key (section 4). A click that acts needs the boundary design of section 4 first. That design is the first build Issue below (P1); every button row assumes it, and is otherwise "today" when the verb exists for the person.

**Top line**

| Feature | Today: source | Missing | Build: where, size, why | Keep or remove |
| --- | --- | --- | --- | --- |
| "N need you" with the repositories | `pending[]` `escalated`; the project through `runs[].project` | - | - | Keep |
| "N failed · organization 1 · loom 3 …" | `anomalies[]`, `runs[]` (`failed` with stage `failed`), `next_work.status`, `makibad.running` | - | - | Keep |
| "N unattended" | derived (4.1) | Whether a notice was delivered, whether the secretary acted (D) | `status_local.rs` from `event_deliveries` and `commands`, M: two fields and their tests, read across three tables | Keep; until D it counts rows with no sign (Q6) |
| "Running · updated 3 s ago" | `makibad.running`, `.heartbeat_at` | - | - | Keep |
| The organization's name | none | E | `status_local.rs`, S: print the base's session name, which `makibad` holds | Keep as a gap until built, or drop the place: nothing else needs it |
| Display: language | `makiba config get conversation_language` (verb `config`, person only; the key is in `door.rs`) | Not on `status --json` (F), but readable by the verb | none needed (S if wanted on `status`) | Keep |
| Display: light or dark | the screen's own setting | - | - | Keep |

**Sidebar: repositories and views**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| One entry per repository, its range, tasks of budget | `projects[]`, `ranges[]` (`id`, `status`, `used.tasks`, `budget.tasks`) | - | - | Keep |
| The state line ("1 needs you · 3 failed") | derived per project from the rows of the top line | - | - | Keep |
| Filter by repository | the screen's own, over rows that all carry a project or are the whole organization's (3.5) | - | - | Keep |
| A proposed replacement beside an active range | `ranges[]` prints both; `range_proposed.replaces` in the events | - | - (layout only) | Keep: the entry lists its ranges when it has two |
| The views' counts | derived from the rows of each view | Pull requests as a field (C) | see C | Keep |
| "Checked GitHub 09:41 · read 10:00" | `next_work.scanned_at`; the screen's own read | - | - | Keep |

**The answer**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| The question, its kind and task, its age | `pending[]` (`id`, `task_id`, `question`, `created_at`, `kind`); `ranges[].stops[].kind` | - | - | Keep |
| The task's title in the heading | `ranges[].items[].title` for a range's task | A one-off task's title (A) | `status_local.rs` `runs`, S: `runs.title` is a column already | Keep; a gap until A |
| Background, if yes, if no, recommended, why | `pending[].context` (five parts); `ranges[].stops[].recommend_answer` | For a question outside a range the recommendation is text only | - | Keep |
| "1 of 3 · Next" | `pending[]` `escalated` | - | - | Keep |
| Answer yes / no, with a note | `makiba answer <id> yes\|no --note` (verb `answer`, person) | - | P1 for a click | Keep |
| Discuss it first | none needed: moves the focus to the conversation | The conversation inside the screen (section 5: M1 to M3 not measured) | the app of section 5; size unknown until the measurement | Keep; it degrades to "go to your terminal" |
| "1 question is with the secretary" | `pending[]` `open` | - | - | Keep |

**Centre: all repositories, a repository's approved work**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| A block per repository: range status, progress cells, tasks and hours | `ranges[].status`, `.items[].place`, `.used`, `.budget` | - | - | Keep |
| Rows that need a look (needs you, did not start, an error) | `ranges[].items[]` with `runs[]` and `anomalies[]` | - | - | Keep |
| Groups by need; the "Below: …" strip | derived from `items[].place`, `.stage` | - | - | Keep |
| "reopened, checks not watched", "checks running / failed / passed" | `pr_closed`, `pr_reopened`, `ci_watch_started`, `ci_completed` (events) and `pr_watches[]` | One field per pull request (C): `items[].stage` `ci` covers three states | `status_local.rs` with `prwatch.rs`, M: a new array folded from `pr_watches` and the merge events, with its tests | Keep |
| Age in review | the newest pull-request event of the task | `items[].since` is the dispatch (J) | `belt.rs` `items`, S: the time the run moved to review | Keep |
| Every item in the order it went in (belt.md 7.1) | `ranges[].items[]` in its order | - | - | Keep: a switch "By need · In order" in the view |
| Instructions | `ranges[].brief_head` | - | - | Keep |
| Hold new tasks | `makiba range pause <id> --reason` (secretary and person) | - | P1 | Keep |
| Start new tasks again | `makiba range resume <id>` (person only, `belt.rs` `range`) | - | P1 | Keep |
| End after running tasks | `makiba range end <id>` | - | P1 | Keep |
| Review the proposal: its read, its brief | `ranges[]` `proposed`: `issues`, `takes`, `candidates[]`, `read_id`, `read_at`, `brief_head` | - | - | Keep |
| Approve exactly this | the door's `range` `approve` takes `read` (`belt.rs`: `int_arg(args, "read")`) | The person's command has no `--read`: `makiba range approve <id>` shows and approves the newest read itself (`cli.rs`, `approve_preview`) | `cli.rs`, S: one argument passed through to the door | Keep; until then the line to type is `makiba range approve <id>` without `--yes` |
| Ask to change it | `makiba send secretary` | - | P1 | Keep |

**Right column**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| A failure: what, where, since | `anomalies[]`, `runs[]`; the project through the run, none for the whole organization | - | - | Keep |
| The reason a launch failed | `worker_spawn_failed.reason`, tied by `spawn_id` to the task's newest `run_taken` or `worker_spawned` | Sure only with G | `status_local.rs`, S: the newest launch's `failed_reason` | Keep |
| Who is on it: waits for your answer | `pending[]` `escalated` of the task | - | - | Keep |
| Who is on it: told, looking | none | D | as above, M | Keep; reads "no sign" until D |
| Ask the secretary to look | `makiba send secretary '<fixed sentence>'` (verb `send`, person) | - | P1 | Keep |
| History | `makiba events --json`, filtered by `task_id` in the screen | `events --task` (I) | `status_local.rs` `events` with `cli.rs`, S: one condition and one flag | Keep |
| Abandon and close… | `makiba close <task> --abandon` (verb `close`, person) | - | P1 | Keep |
| Show the question | the screen's own | - | - | Keep |
| A failure of the whole organization: the lines to type | `makiba down`, then `makiba up`, from the person's login shell, never over the door (spec section 12) | - | none: a line to type by design | Keep |
| Ready to start: Issue, goal clause, size, why, suggested first | `next_work.candidates[]` (`repo`, `issue`, `title`, `clause_id`, `size`, `size_estimated`, `why`, `recommended`) | - | - | Keep |
| Ask to start | `makiba send secretary 'Please take up OWNER/NAME#N.'` | - | P1 | Keep |

**Views behind the sidebar**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| Pull requests: open, failed first, merged today, by hand or through the gate | events `pr_opened`, `fix_pushed`, `ci_watch_started`, `ci_completed`, `pr_closed`, `pr_reopened`, `pr_merged` (`merged_by`, `merged_via`) | A field (C); an open pull request's title (C) | see C | Keep |
| Pull requests: merge state (in conflict, behind) | none | Issue 387 (`mergeable`, `mergeStateStatus`; not merged) | Issue 387, then into C | Keep as a gap |
| "a notice was written to the secretary" (a hand merge, a close) | a `message_sent` by `makibad` with the task, right after the event | Whether it was delivered (D) | see D | Keep, worded as written, not as delivered, until D |
| Issues to start: not ready, and why | `next_work.left_out[]` (`kind`, `reason`) | The title (B) | `triage.rs` `shown`, S: the scan reads the title already | Keep |
| Projects not ranked | `next_work.left_out[]` with a null `issue` | - | - | Keep |
| One-off tasks | `runs[]` not named by a range's item; `terminals[]` for the age | Title, start, Issue (A) | S, as above | Keep |
| Task history: started, began, review rounds, pull request, checks, gate, merged, closed | events `belt_dispatched`, `worker_spawned`, `worker_spawn_verified`, `codex_round_started`, `pr_opened`, `ci_completed`, `acceptance_gate`, `pr_merged`, `run_closed` | "Reported its plan" and "Reported done" when no review answered: `worker_reported` is written only with a review (`door.rs`, `report`); otherwise only a `message_sent` whose kind is not shown | `door.rs` `report`, S: write `worker_reported` for every report, with `review` null when there is none | Keep |

**The conversation, display, keys**

| Feature | Today: source | Missing | Build | Keep or remove |
| --- | --- | --- | --- | --- |
| The secretary's line: state and age | `terminals[]` with `role` `secretary` | - | - | Keep |
| Open the conversation | none: what is said is not on the read surface, by design | The secretary's terminal shown inside the screen | the app of section 5; size unknown until M1 to M3 are measured | Keep folded; in a plain page it says where the terminal is |
| Empty, loading, error, stale | `next_work` null or `error`, `makibad.running` false, the screen's own read | - | - | Keep |
| Keys, focus, filters, the narrow width | the screen's own | - | - | Keep |

**Counts.** 55 rows. **33 are given or derived today** (four of them would be surer or simpler with a small field: G, I, C, F, none needed to draw them). **8 are buttons whose verb the person has today** (answer, range pause, resume and end, send for "Ask the secretary to look", "Ask to start" and "Ask to change it", close with `--abandon`): each is a line to type now and waits only for P1 to be a click. **7 need a small build (S)** before they show without a gap: the organization's name (E), a one-off task's title in the answer and in "One-off tasks" (A), the age in review (J), "Approve exactly this" on the person's command (`--read`), the title of an Issue not ready (B), and the plan and done reports in the history (`worker_reported` for every report). **4 need a medium build (M)**: "unattended", "told" and "looking", and "the notice was delivered" (D, three rows), and an open pull request's title of a one-off task (C). **1 waits for Issue 387** (the merge state). **2 wait for the measurement of section 5** ("Open the conversation", "Discuss it first" inside the screen; both degrade to the person's own terminal). **None is recommended for removal**; the organization's name is the one part that could go without loss, if E is not wanted.

**Build Issues proposed for the "keep" rows**, in the order they unblock the most:

1. **P1. The screen acts as the person** (section 4). **Design first; the Issue opens with the owner's decisions listed first**, each with its options, before any code:
   - *Who holds the person key*: (a) the screen runs the person's `makiba` binary for each act, the key staying where `makiba` reads it (recommended: no new holder); (b) the screen's own process reads `person.key` and speaks to the door itself; (c) a new key of its own for the screen, with its own role and verbs.
   - *How a page's request is told from another local program's*: (a) no page: the screen runs inside the app of section 5 and acts through its terminal only; (b) a page on 127.0.0.1 with a secret the serving process gives the page at load and checks on every request, plus an Origin check; (c) a Unix socket or a named pipe instead of TCP, readable by the person only. Every option must also hold against an agent of the organization, whose sandbox may reach the loopback.
   - *The actor recorded*: (a) `person`, as today, the screen leaving no trace; (b) `person` with a mark that the act came through the screen (`commands.actor` or a field of the request); (c) an actor of its own, `screen`.
   - *Which acts the screen may do at all*: all the person's verbs of section 4, or a subset (answer, range pause, resume, end and approve, send, close with `--abandon`); and whether `close --abandon` and `range approve` need a second step (a confirmation typed, not clicked).
   - *Confirmation and undo*: which acts are confirmed (abandon, approve, end), and whether any can be undone from the screen.
   - *Where it runs* (section 5): decided by the measurement M1 to M4, which the owner runs; P1 waits for it.
   Then M to L.
2. **C. `pull_requests[]` on `status --json`**: every pull request of a run not closed and those merged today, with `ci`, `state`, `merged_via`, `merged_by` and the title (and Issue 387's merge state once it lands). M.
3. **D. Who is on a failure**: `anomalies[].notice` (to whom, queued or delivered, when) and `runs[].secretary_acted_at`. M.
4. **A. `runs[].title`, `.since`, `.issue`, `.range_id`.** S.
5. **`makiba range approve --read <id>`.** S.
6. **`worker_reported` for every report**, so the history shows a plan and a done without a review. S.
7. **B, E, G, I, J**: the left-out title, the organization's name, the launch's failure reason, `events --task`, the time an item went into review. S each; one Issue or five.
