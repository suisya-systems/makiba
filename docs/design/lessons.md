# How lessons reach the work they are about

Issue #330 (a decide Issue, goal clause G9). This note answers its four questions: the form a lesson takes, when it is delivered, how a reader checks that it applies, and how its effect is measured from makiba's own records. It also reviews the curator's launch threshold and candidate gate, and proposes how the two deferred candidates are decided again. The grounds are section 2 (sources), section 3 (makiba's own history) and section 4 (a small replay experiment).

This note changes no code, rule or skill. The build Issues in section 8 do.

## 1. Where lessons go today

| Duty | Owner (file) | What it does |
| --- | --- | --- |
| Write a lesson | `roles/worker/rules.md`, "A note at the end of your report" | A worker ends a done report with a note under four headings, only when it is reproducible, not obvious and not in the docs. |
| Save it | `skills/curate/SKILL.md`, the verb `note` (`src/daemon/door.rs`) | The secretary saves it as `raw/<date>-<topic>.md`, first line `Task: <task_id>`. |
| Launch the curator | `src/daemon/curator.rs` (`RAW_THRESHOLD` = 5) | After a close, 5 or more raw notes new since the last reported cycle launch one curator cycle. |
| Merge and propose | `roles/curator/rules.md` | Curated notes `curated/<pattern>.md`; a candidate when notes from 3 or more different tasks name a pattern. |
| Decide | `skills/curate/SKILL.md`, the verb `candidate` | The owner approves, rejects, folds into a skill, or defers. Only an approved candidate becomes a skill. |
| Brief a worker | `skills/delegate/SKILL.md` (one-off), `skills/conveyor/SKILL.md` and `belt::brief` in `src/daemon/belt.rs` (a range's fixed `brief_head`) | The brief and, for an Issue, its text. |

Where the Issue's text and the records disagree:

- The Issue says a worker's brief carries no lesson. A range's `brief_head` does carry lessons, written by hand: the head of range 13 names the trailer paragraph, the helper inside the declared test file, `tests/common/exe.rs`, comparing failing sets with the base in a detached worktree, and reading the nearest module's head comment. Each of these is a curated note's lesson. That channel has no link to the note, is the same for every Issue of the range whatever it touches, is fixed for the range's life, and nobody measures it.
- `docs/design/notes-pass.md` put four lessons into `roles/worker/rules.md` on 2026-11-22. So some lessons do reach every worker, always loaded. Section 3.2 shows what that did.
- The Issue says tasks of the same night count separately. The curator's rule 3 counts "different tasks"; it does not say "different Issues". Section 3.3 shows two tasks on one piece of work counted as two.

## 2. What the sources say

Each claim names where it was read. Two sources could not be read: the Weber, Aha and Becerra-Fernandez papers on lessons-learned systems (every copy tried answered 403), and the APQC study (only search snippets were seen). Nothing below rests on them.

### 2.1 Human organisations

NASA Office of Inspector General, *Review of NASA's Lessons Learned Information System*, IG-12-012, 6 March 2012 (oig.nasa.gov/docs/IG-12-012.pdf, read whole as text):

- "NASA program and project managers rarely consult or contribute to LLIS even though they are directed to by NASA requirements and guidance." Of 28 project managers surveyed, 16 (57%) used LLIS during the project life cycle and 12 (43%) contributed to it.
- The reasons given: the system is "outdated, is not user friendly, and does not contain information relevant to their projects"; "most of the information in the system did not apply to their projects because their projects were unique"; "difficult to find information in the system relevant to the specific issues they faced". One manager: "[We] didn't find anything useful, but we were able to say that we had checked this box."
- "6 of 10 Centers did not cross-reference lessons learned to their management or engineering standards, thereby further limiting the effectiveness of lessons learned."
- A letter from the Chief Engineer in 2009 asking for lessons to be captured while memories were fresh "did not result in a measureable improvement".

What makiba takes from it: a database of lessons that one must remember to search is not searched; relevance is the reader's first complaint; and a lesson that is not written into the standard the work follows (in makiba: a check, a rule, a skill) stays outside the work. Exhortation is not measured improvement.

### 2.2 Agent memory research

MemDev-Bench (Chen et al., arXiv 2610.27524, v1, 20 October 2026; read through Appendix C):

- Setup: code agents (Claude Code on DeepSeek-V4-Flash) build memory from earlier tasks of the same repository and are measured on a later task; 344 base tasks and 2,987 historical tasks on SWE-bench Verified (section 3).
- Memory helps a little and hurts some: on tasks failed without memory, resolved rates rose from 35.3% (no memory) to 36.8 to 42.6% (Table 2); on tasks solved without memory, some memory systems fell below the 94.0% baseline, raw trajectories in the noisy and whole-repository histories to 92.7% and 90.0% (Table 3). "Regressions offset gains, especially on already solvable tasks" (section 4.1).
- Failures caused by memory: 115 cases on 76 tasks that were always solved without memory; "agents tend to follow memory as a solution rather than reassess it against the current task context"; misapplied transfer is the commonest mode, and "relevance alone does not ensure reliable reuse" (section 4.3). Unrelated history raises harm (mean 7.1% to 8.0% from relevant to noisy history, section 4.1).
- Raw against summary: the paper has no controlled accuracy comparison. Raw trajectories cost fewer steps (about 34% fewer than no memory, 29 to 35% fewer than the summary systems, section 4.4), and the abstraction "can lose information about the sequence of code inspection, failed attempts, revisions, and test outcomes".
- Its recommendation: "preserve the solving process behind historical solutions while making their applicability explicit" (section 4.4); agents "should reassess historical guidance when current evidence no longer supports it" (section 5).

The Issue's summary of MemDev-Bench held only in part: the paper shows memory followed without reassessment causing failures, and unrelated memory raising harm; it does not test "with and without an applicability check" as conditions, and it does not show raw records beating summaries in accuracy, only in cost and in kept detail.

ACE, *Agentic Context Engineering* (Zhang et al., arXiv 2510.04618 v3, ICLR 2026; read whole):

- Context collapse (section 2.2): when a model rewrites the whole accumulated context at each step, one step took a context of 18,282 tokens at 66.7% accuracy to 122 tokens at 57.1%, below the 63.7% of no adaptation at all. "A fundamental risk of end-to-end context rewriting."
- Brevity bias (section 2.2): optimisation drifts toward short, generic instructions that drop domain detail.
- The remedy (section 3): a playbook of itemised bullets, each with an id and counters of how often it was marked helpful or harmful; new bullets are appended, existing ones updated in place, duplicates pruned; the merge is "lightweight, non-LLM logic". Without incremental updates the gain fell from +17.0 to +3.6 points (Table 18).
- Its limit (section 5): it relies on a reasonably strong reflector and on the quality of feedback; without a ground-truth or execution signal the context "can be polluted by spurious or misleading signals" (section 4.4).
- The paper does not say how a subset of the playbook is chosen for a query; the token counts suggest the whole playbook is in context.

### 2.3 Products

Read at docs.devin.ai, cursor.com/docs/context/rules and code.claude.com/docs (2026-11-26):

| Product | Always loaded | Loaded on a trigger | How relevance is decided | Size guidance |
| --- | --- | --- | --- | --- |
| Devin Knowledge (deprecated for Skills) | items pinned to a repository | "retrieves Knowledge when relevant, not all at once" | a required "Trigger Description" | "a handful of sentences"; split into smaller items |
| Devin Skills | name and description of every skill | the body, when invoked | the model reads the description | none stated; usage counted in analytics, effect not |
| Cursor rules | "Always Apply" | "Apply to Specific Files" when a file matching `globs` is in context; "Apply Intelligently" from a description; manual by @-mention | glob, or the model reads a description | "Keep rules under 500 lines"; "Split large rules" |
| Claude Code | CLAUDE.md files; rules in `.claude/rules/` without `paths` | a rule with `paths` globs "loads when Claude uses the Read, Write, or Edit tool on a matching file"; a skill's body when used | glob, or the model reads a skill's description | "target under 200 lines per CLAUDE.md file"; "Move instructions that matter for only part of the codebase into path-scoped rules" |

All three products split knowledge into a small always-loaded index and bodies loaded on a trigger, and two of them trigger by file path. None of them measures a lesson's effect; Devin counts uses.

## 3. makiba's own history

Read from the knowledge folder on 2026-11-26: 40 curated notes (222 KB), 125 raw notes (156 KB; 34 in `raw/`, 91 in `raw/archive/`), 27 candidate entries.

### 3.1 Size

The curated notes are about 55,000 tokens. Delivering all of them to every worker would put a quarter of a 200,000-token window into each brief, and by MemDev-Bench's section 4.1 unrelated memory would raise harm. Any delivery is a selection.

### 3.2 A lesson in the always-loaded rules still recurs

The worker's rules have said since 2026-11-22 (`docs/design/notes-pass.md`) that a hook refuses a whole call by its text and that text naming a guarded command goes by file. From 2026-11-24 to 2026-11-25 notes from at least 20 different tasks describe a hook refusing a call for its text (a name assigned twice in a heredoc, the word `gh` or `hc` in a heredoc, a grep pattern or a commit message). Every refusal message already names the line and what to do instead (`src/hooks/mod.rs`, `Refusal`: "the reason with what to do instead").

So the always-loaded rule did not prevent the refused calls, and the check's own message did deliver the lesson at the moment of need: each note says the worker took the alternative and went on. The cost of each recurrence is one refused call. What did not stop is the notes: workers kept writing a lesson that a rule and a check message already held, and it fed four candidates (`text-hook-exceptions-do-not-converge`, `scripted-edits-under-text-reading-hooks`, `worker-restriction-misread-as-other-fault` twice).

### 3.3 The two deferred candidates

`sqlite-state-db-changes` (curated note, 6.8 KB): tasks r2-270, r2-282, r2-288 and park-283 up to the rejection on 2026-11-23; r8-426b after it. In both later tasks the failure was caught by a test, not by a reader: park-283's v10 copy failed `writer_migrates_a_pre_m2_database` and `writer_migrates_a_pre_ledger_database` in `tests/db_writer.rs`; r8-426b's scalar subquery NULL was caught by `tests/restart_no_report_yet.rs`. Its trigger is a set of files: `src/db/**`, `src/db/writer.rs`, `src/daemon/anomalies.rs`.

`write-down-a-records-whole-life-first` (curated note, 9.3 KB): 9 tasks. Its failures were caught by reviews, at the cost of rounds: r2-282 four diff rounds, belt-ahead-284 rounds 3, 5, 6 and 7, r11-502 a design-review blocker and two holes in diff round 1. Its trigger is a phase (designing a record that several writers create, change, withdraw or count), not a file. Its second entry rests on r6-405b and r6-405c, two successive tasks on one fold of `src/status_local.rs`, counted as two tasks.

## 4. The replay experiment

Two past design steps were run again from the base their task started on, each three times: with no lesson (A), with the curated note as it stood before the task (C: the current note with every line naming that task or a later one removed), and with the raw notes the curated note listed then (D). Each run was a fresh agent of the same model as the workers, read-only in a detached worktree, told not to read other commits, asked for a design of at most 500 words. The criteria were written down before any answer came back.

| Scenario | Primary criterion (the failure the real task met) | A none | C curated | D raw |
| --- | --- | --- | --- | --- |
| S1: park-283, migration v10 rebuilding `runs` (base 60c3f4a) | handle the older shapes of `runs` that `tests/db_writer.rs` still migrates | met: found the minimal M1 table, chose the copied columns from `pragma_table_info` | not met: named the pre-M2 and pre-ledger databases as cases, but copied a fixed list of 21 columns, which fails there with "no such column" (the real failure) | met: found the gap, crediting the r2-270 note's "test from an older, a half-migrated and a bad-row database" |
| S2: r11-502, a stored handover given once (base af171d6) | "received" judged by a verified launch (`verified_at`), not by `stage` or timestamps | met | met (credited "read the life from the code that writes it") | met (credited "record a state move at its single writer") |

Secondary criteria: all six turned foreign keys off around the rebuild or handled a failed launch; all three S2 runs covered makibad's own relaunch; only C of S2 said plainly that a handover stored before any secretary ran reaches the first launch.

What it shows, within its limits:

- On these two steps a delivered lesson did not change whether the real failure was avoided. A capable reader who reads the code at the moment of need found the edge without the lesson in both.
- The one miss was the summary: the curated note gave the reader the right word ("older shapes") and the reader stopped at naming it. The raw note, which kept how the earlier failure was found, led to the check. This agrees with MemDev-Bench's section 4.4 and ACE's brevity bias; with one run per cell it is a sign, not a measurement.
- In the real history both failures were caught by a check (S1: a test; S2: the design review). The lesson's value was at most the round it would save.

Limits: one run per cell; both task texts were written for the replay and point at the files to read (S1 names `tests/db_writer.rs`; S2 asks for "every way a launch can start, fail or be replaced"), which favours condition A; the curated text of C is a reconstruction, since curated notes have no history.

## 5. The design

### 5.1 The form of a lesson: chosen by what can hold it

A pattern that qualifies (section 6) is given one form, the first that fits, in this order:

1. **A check** (a test, a lint in `tools/`, a hook's refusal message, the acceptance checker): when the failure can be detected mechanically. This is the owner's choice of 2026-11-23 and what the history shows works (section 3.2, 3.3). A check is built by a code task and lands like any change.
2. **A path rule**: when the trigger is a set of files of one repository. A Claude Code rule in that repository's `.claude/rules/<pattern>.md` with `paths` globs, loaded when the agent reads or edits a matching file (section 2.3). Its body is short: the When line, the judgments, and the raw notes to read (by name) for how earlier failures were found. It is a tracked file, so the curator's rule 7 (nothing operator-private) holds for it as for a skill.
3. **A brief item**: when the trigger is a phase of the work (a design on a record, a test commit, a review). An item of `skills/delegate/SKILL.md` that the secretary puts into the brief when the phase is in the task, and that the review then reads against.
4. **A skill**: when the lesson is a procedure of several steps with its own trigger.
5. **A delivered note**: everything else. The curated note stays, and is delivered by section 5.2.

A pattern already held by a check or a rule is not a candidate: the curator marks it `held: <the check or rule>` in its curated note and the notes that name it are archived. That ends the noise of section 3.2.

### 5.2 When a lesson is delivered

- **At the moment a file is touched**: the path rule (5.1, form 2), by Claude Code itself; makiba adds no watcher.
- **At the start of the task, selected by the worker**: one index, `curated/INDEX.md`, kept by the curator (one line per curated note: its `When:` line and its file, about 40 lines). The worker's rules say: before the design (or, with no design, before the first change), read the index, open each note whose When holds for this task and the raw notes it names, and check them against the code. The worker selects, not the secretary, because a range's tasks are dispatched by makibad with a fixed head and the Issue's text (`belt::brief` in `src/daemon/belt.rs`; `skills/conveyor/SKILL.md`): there is no step at which the secretary could choose per Issue, and the worker is the one reader who has both the Issue and the code. A one-off brief may still name a note (`skills/delegate/SKILL.md`); the worker treats it as one more selection. A range's `brief_head` keeps only what holds for every Issue of the range; the lessons now written into heads by hand go to the index.
- **Where the index is**: a worker's environment does not carry `MAKIBA_KNOWLEDGE_DIR` today (only `src/daemon/curator.rs` reads it). So makibad, which composes every brief, appends one line naming the index's absolute path: in `belt::brief` for a range task and in the door's `dispatch` (`src/daemon/door.rs`) for a one-off, the two places that already append the Issue's text. This is the design's one code change (build Issue 2); the worker's sandbox can read the knowledge folder (it is not among the denied paths).
- **At review**: a brief item (5.1, form 3) is a sentence the design must answer, so the design review reads the answer.

Nothing is delivered whole. Raw notes are named beside the curated note because the replay's only miss was the summary. A rule or a curated entry names a raw note by its file name only (`2026-11-23-sqlite-open-takes-write-lock.md`), never by `raw/` or `raw/archive/`: the verb `note` keeps file names unique across both folders (`skills/curate/SKILL.md`), so a reader finds it in either, and the curator's archiving (its rule 5) breaks no tracked reference.

### 5.3 How applicability is checked before use

The reader checks, not the writer: the worker's rules gain one sentence, "a lesson you selected or were named is checked against the code before you use it; your design note and your done report end with a line `Lessons:` naming each such note as `used`, `not applicable` (with why in a few words) or `wrong` (with the fact that contradicts it), or `Lessons: none matched`". The replay's agents did this when asked and each dropped the points that did not apply (S1: the trigger and JSON points; S2: the pull-request and fold points).

### 5.4 How the effect is measured from makiba's records

The record is the raw note, the one durable channel the curator reads. A report's text is kept only in `messages`, addressed to the secretary; the `worker_reported` event carries `task`, `state` and `review`, not the text (`src/daemon/door.rs`, `report`); and the curator has no tool that reads either. So:

- **Use records.** When a done report carries a `Lessons:` line, the secretary saves it with the verb `note`, as it saves a lesson note and at the same point (before the close, `skills/curate/SKILL.md`): topic `lesson-use`, the four headings with the `Lessons:` line verbatim under `## Facts` and the words "a use record (Issue 330)" under the other three, and `idem` the report's message id, so a retry or a secretary renewed between report and save writes it once. The verb writes it as `raw/<date>-lesson-use[-n].md` with its `Task:` line. The curator reads the whole pile every cycle (its rule 1), so no cursor is needed; it counts use records and archives them, and they never name a pattern by themselves. A `wrong` in a use record makes the curator correct the entry it names or mark it `wrong:` with the contradicting fact, in the same cycle.
- **The Issue of a note.** Counting different Issues (section 6) needs an Issue the curator can read. The verb `note` writes, after `Task:`, a line `Issue: <owner/name>#<n>` from the run's recorded Issue when the run has one (the `issue` given at dispatch, or the range's), and no line otherwise; makibad has this identity, the curator does not. A note without the line counts by its task id, which covers every note saved before the change: they are not rewritten (a saved note is never edited).
- **Landing dates** of a form are the merge of its pull request, which the secretary gives in the cycle's launch message or the curator reads from the form's line in its curated note (written when the secretary tells it).

For each pattern the curator's report gives:

- **recurrence after its form**: raw notes naming the pattern saved after the date its form landed, split into tasks whose brief named it (or that touched its rule's files) and tasks it did not reach;
- **use**: counts of `used`, `not applicable` and `wrong` from the `Lessons:` lines the secretary passes it.

Two rules follow, applied by the curator and put to the owner as candidates:

- A pattern that recurs in 3 different Issues that it reached moves one step up the list of 5.1 (a delivered note to a brief item or a path rule; a rule or item to a check).
- A lesson marked `not applicable` in 3 different Issues has its When line narrowed.

### 5.5 Curated notes are extended, not rewritten

ACE's context collapse (section 2.2) is a risk for the curator's rule 6, which lets it restructure a curated note freely. A curated note becomes a list of entries, each with a short id, its sources and its date; a cycle appends entries, edits one in place or marks one `held:` or `wrong:`, and never rewrites the note whole. Rule 6's "nothing of substance goes silently" stays.

## 6. The curator's threshold and gate

- **Launch at 5 raw notes** (`RAW_THRESHOLD`): kept. Notes arrive at about 25 a day (125 in five days), so a cycle runs several times a day; the threshold bounds cost, not delivery, since the delivery of 5.2 reads what is already curated.
- **Candidate at 3 different tasks**: changed to **3 different Issues** (a task without an Issue counts by its task id). Two tasks on one Issue or one piece of work (r6-405b and r6-405c) are one ground. Tasks of the same night still count when their Issues differ: the trailer and test-helper lessons recurred across different Issues of one night, which is the signal the gate is for.
- **A candidate names its form**: an entry gains a line `form: check | rule | brief-item | skill | note` beside `skill`, so the owner decides the form, not only "skill or not". Whether the verb `candidate` reads such an entry unchanged is for the build Issue to establish (it reads entries by `<key>: <value>` lines; `src/daemon/door.rs`, `candidate_field`).
- **Each form is fulfilled and checked by its own evidence.** `approved` is recorded for every form; what follows depends on the `form` line, and the curate skill's "check the chain" step names, for each, what must be merged before the secretary tells the owner it is in:

| Form | Fulfilled by | Done when |
| --- | --- | --- |
| check | a code task in the project the check guards | its pull request is merged, and its test shows the check refusing the case the notes describe |
| rule | a task adding `.claude/rules/<pattern>.md` to the repository | merged; its `paths` cover the files the When names |
| brief-item | a task editing `skills/delegate/SKILL.md` (or the role rules) | merged; the item's sentence states the When |
| skill | a task adding `skills/<skill>/SKILL.md`, as today | as today |
| note | no task: the curator adds the INDEX line | the line is in `curated/INDEX.md` |

- **A held pattern** (`held: <check>`) stays measurable where the check leaves a record (a test that fails in CI is in the pull request's checks); a hook's refusal leaves none in makiba's records, so a hook-held pattern is not measured, which is accepted (section 3.2: its cost is one refused call). The worker's rule against noting a held lesson keeps one exception: a held lesson that did not hold (the check's message misled, or the check let the case through) is noted, since that is a defect of the check.

## 7. The two deferred candidates under this design

| Candidate | Proposed form | Why |
| --- | --- | --- |
| `sqlite-state-db-changes` | **rule**: `.claude/rules/sqlite-state-db-changes.md` in makiba with `paths` `src/db/**` and `src/daemon/anomalies.rs`, naming the raw notes | Its trigger is a set of files (3.3). Its two later failures were caught by tests, so no new check is proposed; the rule saves the round. The verb has no decision "into a rule", so the entry is recorded `rejected` with the reason "held as a path rule (Issue 330)" once the rule lands. |
| `write-down-a-records-whole-life-first` | **brief item**: `skills/delegate/SKILL.md` gains "a design on a record or derived state that several writers create, change, withdraw or count carries the record's life: what creates it, withdraws it, may create it again, counts it, and when it leaves each list a reader sees, read from the code that writes it" | Its trigger is a phase (3.3); its cost was review rounds; the replay found readers already do it when the task asks for every path (S2), so asking for it in the brief is the lever. Recorded as `into-delegate`. |

Both decisions are the owner's; the secretary records them with the verb `candidate` after this note is agreed.

## 8. Proposed build Issues

1. **Curator: index, entries, held, form, gate by Issue.** `roles/curator/rules.md` and `skills/curate/SKILL.md`: keep `curated/INDEX.md`; entries with ids, appended not rewritten (5.5); `held:` marks and archiving (5.1); the `form` line, the fulfilment table and the gate by different Issues (section 6); use records and the recurrence and use counts in the report (5.4). Establish whether `candidate` accepts the `form` line unchanged; if not, the verb and spec section 12 change with it. Documents only, unless the verb must change.
2. **Delivery: the index line in every brief, the worker's selection and the `Lessons:` line.** Code: `belt::brief` (`src/daemon/belt.rs`) and the door's `dispatch` (`src/daemon/door.rs`) append one line naming the index's absolute path (5.2); a declared test that a range brief and a one-off brief carry it; spec section 12 if it names the brief's parts. Documents: `roles/worker/rules.md` (select from the index, check, the `Lessons:` line, no note for a held lesson that held), `skills/delegate/SKILL.md` (a one-off brief may name a note), `skills/conveyor/SKILL.md` (a brief head keeps only what holds for every Issue), `skills/curate/SKILL.md` (save a `Lessons:` line as a use record).
3. **The Issue of a note.** Code: the verb `note` (`src/daemon/door.rs`) writes `Issue: <owner/name>#<n>` after `Task:` when the run has an Issue; spec section 12 and the curate skill say so; a declared test with and without an Issue. Existing notes are not changed.
4. **Path rules: a probe, then the first rule.** Establish on the pinned Claude Code, in a worker's worktree under its hooks and sandbox, that a `.claude/rules/*.md` with `paths` loads on Read or Edit of a matching file (a probe in the manner of `docs/design/probes.md`); if it does, add `.claude/rules/sqlite-state-db-changes.md`. If it does not, the form falls back to a brief item selected by the Issue's named files.
5. **The brief item for a record's life.** The item of section 7 in `skills/delegate/SKILL.md`. Documents only; may be folded into Issue 2.

## 9. What this design does not do

- It adds no watcher, poller or new record to makibad. Its code is two small additions to owners that exist: one line appended by the two functions that already compose briefs (build Issue 2), and one line written by the verb that already writes notes (build Issue 3). The index is a file of the knowledge folder, kept by the curator; the path rule is loaded by Claude Code.
- It keeps no state in memory beside the ledger.
- It parses no file format for a safety judgement.
- It does not re-run the six candidates rejected on 2026-11-23 other than the two the owner deferred.
