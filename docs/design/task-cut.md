# Who cuts work into tasks (Issue 343): a study and a design

Decide Issue 343. Goal clause G2: the flow of work does not wait for the person's turn. A task that is too large holds a slot for days of review rounds, forces orderings on the person (Issue 284's #336 merging only after #332), and lands only by the acceptance lift; a task that is too small pays the fixed cost of a task (a test commit pushed alone, a CI failure, a review, the gate) for little. This note studies what is known (sections 1 to 3), then names an owner of the cut, when it is made and re-made, the signals for a split, and how a split keeps the declared-test discipline (sections 4 to 7). Nothing is built until the owner approves; section 8 lists the build Issues.

The owner's direction, from the Issue: no short-cut stop such as a line-count threshold; study first.

## 1. Human teams

### 1.1 Small batches, and why

- **Batch size and flow.** Reinertsen, *The Principles of Product Development Flow* (Celeritas, 2009), chapter 5 "Reducing Batch Size": smaller batches shorten cycle time, speed feedback and cut variability; the best batch size is an economic trade-off between the holding cost (delayed feedback, delayed value) and the transaction cost of each batch (preparing and finishing it), and making small batches pay means lowering the transaction cost. This is the frame this note keeps: in makiba a task's transaction cost is high and fixed (section 3.4).
- **DORA.** The capability "Working in small batches" (dora.dev/capabilities/working-in-small-batches) names fast feedback as the reason, finds that it predicts delivery and organizational performance, and lists two pitfalls: splitting work too coarsely, and regrouping small batches before they go downstream. Its research also reports that small batches strengthen the effect of AI adoption on product performance.
- **Small changes, not a line count.** Google's engineering practices, "Small CLs" (google.github.io/eng-practices/review/developer/small-cls.html): a change should do one self-contained thing, include its tests, and "small" is the conceptual focus of the change, not a function of its line count. Ways to split: stacking (the next change on top of the one under review), by files that need different reviewers, horizontally (a stub between layers), vertically (full-stack features in parallel).
- **Change size at scale.** Sadowski et al., "Modern Code Review: A Case Study at Google", ICSE-SEIP 2018 (9 million reviewed changes): the process rests on small changes, one reviewer and quick iterations. (The median of about 24 changed lines often quoted from it was seen here only in a secondary summary; this note does not use the number.)
- **Review effectiveness.** SmartBear's report of the Cisco MeetingPlace review study: defects found per line fall as more than about 200 to 400 lines are read at once, and as the reading rate rises. Seen only through SmartBear's own material, not a peer-reviewed paper; it is used here only for the direction (a reader finds less in a larger change), which section 3 sees in makiba too.
- **Size and merge time: contested.** Kudrjavets et al., "Do Small Code Changes Merge Faster? A Multi-Language Empirical Investigation" (MSR 2022, arXiv 2203.05045, 845,316 GitHub pull requests and 401,790 Gerrit and Phabricator reviews): size does not relate to time to merge. Gousios et al. (ICSE 2014) found change size among the factors of a pull request's acceptance. So size alone is not a reliable predictor of how a change goes; what a change *is* (how many concerns it carries) matters more. This is one reason for the owner's "no line-count threshold".

### 1.2 How work is sliced

- **Vertical slices.** A slice goes through every layer needed to show one behaviour; the INVEST criteria (Wake, 2003: Independent, Negotiable, Valuable, Estimable, Small, Testable) describe a good one. Cockburn's "elephant carpaccio" exercise trains cutting a feature into many such thin slices.
- **Splitting patterns.** Lawrence, "Patterns for Splitting User Stories" (2009; later the Humanizing Work guide): workflow steps, business rule variations, major effort, simple/complex, variations in data, data entry methods, defer performance, operations, break out a spike. The test of a split is that each part is still a vertical slice and that the split shows which part is worth less (it can wait or go).
- **Testable is the hinge.** In makiba every build task has a declared acceptance test written before the change. "Testable" in INVEST is therefore not one criterion among six here: a slice is a behaviour one declared test can show failing at the test commit and passing after the fix.

### 1.3 Who owns the cut, and when

- **Scrum.** The Scrum Guide (2020): the Product Owner is accountable for the Product Backlog (its order and content); refinement, the breaking down of items into smaller, more precise ones, is an ongoing activity of the Scrum Team, and "the Developers who will be doing the work are responsible for the sizing". The owner of value orders; the people who will build it cut and size, because they see the work.
- **Shape Up.** Singer, *Shape Up* (Basecamp, 2019): senior people shape the work (bounded, at the right level of abstraction) before it is bet on; the team building it maps the *scopes* itself while working ("scope mapping isn't planning... you need to walk the territory before you can draw the map"), the task list grows as the team discovers work, and the team *hammers* the scope (nice-to-haves marked and usually cut) to finish in the time box. The cut is made twice: coarsely when shaping, and finely by the builders once they see the code.
- **Trunk-based development and the DORA research** keep branches short-lived (a day or less) and changes small; the cut there is the author's, every day, against the rule that main stays releasable.

What the human practice agrees on: the cut is not a number; it is owned by the people who will do the work, checked in refinement or review, and made again when the work shows itself (discovered scopes). The person who owns value decides order and what is worth doing, not the size.

## 2. Agent planning research

- **Task length and success.** Kwa et al. (METR), "Measuring AI Ability to Complete Long Tasks" (arXiv 2503.14499, 2025): an agent's success falls with the length of the task, measured in the time a human expert takes; the length at which a model succeeds half the time doubled about every seven months from 2019 to 2025, and the 80% horizon is several times shorter than the 50% horizon. For a pipeline that needs a task to pass, not to pass half the time, the cut matters more for an agent than for a person: a task near the agent's horizon fails often, and fails in more complex ways.
- **Patch size and difficulty.** SWE-bench (Jimenez et al., ICLR 2024, arXiv 2310.06770): reference fixes average 1.7 files, 3.0 functions and 32.8 changed lines; in SWE-bench Verified, the tasks annotated as quick have markedly smaller patches than the longer ones. Benchmarks of real repository work with larger patches report far lower resolve rates. The tasks makiba's workers take are one to two orders of magnitude larger than these (section 3).
- **Decompose as needed.** Prasad et al., "ADaPT: As-Needed Decomposition and Planning with Language Models" (Findings of NAACL 2024, arXiv 2311.05772): a planner decomposes a sub-task only when the executor fails at it, recursively; this beat plan-everything-first and execute-only baselines by up to 28.3, 27 and 33 points on ALFWorld, WebShop and TextCraft. The cut is made when execution shows it is needed, and adapts to the executor's ability.
- **Plans need an outside verifier.** Kambhampati et al., "Position: LLMs Can't Plan, But Can Help Planning in LLM-Modulo Frameworks" (ICML 2024, arXiv 2402.01817): a model's plan is a candidate, to be checked by external critics and verifiers (human or code) in a generate-test loop. A cut proposed by one agent needs a second reader, as makiba already gives a design.
- **Orchestrator and workers.** Anthropic, "Building effective agents" (December 2024): when subtasks cannot be predicted in advance (coding, where the files to change depend on the task), an orchestrator decides them from the input rather than following a fixed list. Plan-and-execute schemes (e.g. Wang et al., "Plan-and-Solve Prompting", ACL 2023) plan first; re-planning schemes plan again on failure.

What the agent research agrees on: shorter tasks succeed far more often; a fixed plan made before the work is seen is weaker than decomposing when the work shows it is needed; and an agent's plan is a candidate that a verifier checks.

## 3. makiba's records

### 3.1 What could be read, and what could not

- A worker cannot read `state.db` or `makiba events` (its sandbox denies them), and neither can the secretary's tools (it answered so, message 1994). So review rounds per task, CI cycles per task and escalations per task were **not** counted from the ledger. Issue 51's report (`docs/reports/self-hosting-week.md`) counts the week as a whole, not per task. The full per-task table needs the person to export `makiba events --after 49 --json`; this study does without it and says where that weakens it.
- What could be read: every task branch is kept at `origin` (125 `makiba/*` branches), and every landing is one squash commit on `main` whose subject names the Issue and the pull request. From these: the landed change's size (files, lines), the declared test commit's size, the commits after the test commit (each a fix: a review finding, a CI failure or a base merge's fallout; base merges counted apart), and the number of branches per Issue (a rebuild: `-redo`, `-v3`, `b`, `c`).
- The secretary's own record (message 1994, a colleague's report, checked against git where it could be): r10-475 needed 9 diff review rounds and landed by the acceptance lift; r11-506 also landed by the lift; r11-502 stopped at the gate for list additions to two tool-list tests. Git agrees on 475: 8 fix commits after its test commit, each naming a different reader or race (below).

### 3.2 Size and the fixes after the test commit

The 65 landed tasks with a declared test since 2026-11-22, by the size of the landed change (insertions plus deletions of the squash commit; the test is included):

| Landed change | Tasks | Fix commits after the test commit, median (max) | Tasks with 3 or more fixes | Rebuilt (more than one branch) |
| --- | --- | --- | --- | --- |
| under 300 lines | 17 | 1 (5) | 1 | 4 |
| 300 to 999 | 30 | 1 (4) | 7 | 10 |
| 1000 to 1999 | 12 | 2.5 (6) | 6 | 2 |
| 2000 and more | 6 | 5 (8) | 4 | 1 |

Rank correlation (Spearman) of the change size with the fix commits: 0.42; of the files changed with the fix commits: 0.39. The six largest are Issues 475 (3325 lines), 282, 284, 432, 431 and 388.

Read: fixes rise with size, steeply above about a thousand lines, but size explains only part (0.42), as Kudrjavets et al. found for merge time. Rebuilds do not rise with size: 17 Issues were rebuilt at every size, and the branches show the causes were mostly the process (a test commit pushed with its fix, a base that moved under a test commit), not the scope. This table is the evidence the note uses; it is not a threshold, and a line count stays out of the mechanism (section 5).

### 3.3 What the large tasks carried

The fix commits of the largest tasks name what each round found. They show that the tasks that went wrong carried **several concerns**, each of which drew its own rounds:

- **Issue 284 (PR 336)**, belt step 4: the Issue bundles two features, `design_ahead` and `follow_ups: inside` (belt.md section 8, step 4). Its test commit 3d0f0bb adds 725 lines in one file, `tests/belt_ahead.rs` (the Issue says "in two tests"; git shows one test file). Six fixes, all on the design-ahead half (closed pull requests coming back, old merges, parked runs, moot questions); and the ordering decision (#336 after #332) came from that half alone.
- **Issue 475 (PR 500)**, "makibad judges queue readiness and updates a branch itself": a judgment, a reader of `.github/` YAML, git readers, and an update act that pushes. Eight fixes: "stricter .github/ reader", "readers: own refs only, bounded git", "updates merge only the judged head", "anchors in flow collections", "every .github/ change counts", "bound to their whole context", "finds its own push after a restart". Three concerns (reading a file format, judging, acting) in one declared test; it landed by the lift.
- **Issue 282 (PR 323)**, belt step 2: the stops makibad writes, the budgets, the pause and the rework counts (belt.md section 8, step 2, lists five things). Three branches and seven fixes, each on a different one of those things.
- **Issues 431 to 435**, the merge act, were cut by their design note before building (`docs/design/merge-act.md` section 11: six build Issues, each with its own acceptance and dependencies). Each still landed at 200 to 2700 lines, but with 1 to 6 fixes and no rebuild, and they ran in parallel where their dependencies allowed. A cut by concern made in the design worked; it was made by the worker who wrote the design, checked by the design review, and approved by the owner with it.
- **Issue 169 (PR 305)**, the screen design, a document: 13 commits after its first, each a round of the person's reading. A document's rounds are the person's, by design (screen.md); size is not its question.

### 3.4 The fixed cost of a task in makiba

Every build task pays, whatever its size: a design and its review round; a test commit pushed alone and seen failing in CI; a fix; diff review rounds; the gate; a merge and its CI; a close with its retro. A split doubles these. And a declared test cannot change after its test commit, so a cut made after it is expensive: the part the declared test needs cannot leave the task. In Reinertsen's terms the transaction cost is high, so the cut must be by concern, not as fine as possible; and the cheap moment to cut is before the test commit.

## 4. The question, answered

Today nobody owns the cut: Issues are written by the steps of a design note (belt.md section 8 put two features in step 4), the secretary dispatches one Issue as one task (the delegate skill: "Give each task one worker with an exact scope", with nobody checking that scope's size), the triage's effort is an estimate (`src/work_discovery/effort.rs`, marked "(estimated)"), and the worker builds what the Issue names (the brief: "Keep the change to what the Issue asks").

From the study: the cut is owned by whoever will do the work and sees the code (Scrum, Shape Up); it is made when the work shows itself, not only up front (Shape Up, ADaPT); a proposed cut is checked by a second reader (LLM-Modulo, code review); and it is by concern and behaviour, not by lines (Google, Lawrence, DORA's pitfalls, section 3.3).

## 5. The design

### 5.1 The owner: the worker who writes the design, checked by the design reader, approved with the design

- **Owner.** The worker who writes the design owns the cut of its work. It is the only role that has read the code the work touches before the test commit. This holds for a decide Issue (whose design proposes the build Issues) and for a build Issue (whose design may split it).
- **Checker.** The design reader checks the cut as part of the one design round (the review module `src/daemon/review.rs`, which owns the reader's prompt). The reader is told to name a Major when the design carries more than one concern of section 5.3 without saying why they cannot be split.
- **Approver.** Whoever approves the design approves its cut, in the same answer: the secretary, or the person where the brief says the design is theirs (screen Issues, trust-boundary Issues). No new approval step.
- **Filer.** The secretary files the slices that leave the task, with the issue skill, from the texts the worker proposes in its blocked report (a worker cannot write to GitHub).
- **Not the owner.** The secretary at dispatch does not cut: it has not read the code, and a cut by the Issue's text alone is what made Issue 284. makibad does not cut: a cut is a judgment (G5 asks code to hold what must not happen; a task's size is not that). The triage's effort stays an estimate for the order of work.

### 5.2 When the cut is made and re-made

| Moment | Who | What happens |
| --- | --- | --- |
| A design proposes build Issues (decide Issues, design notes with a build order) | The designing worker | Each proposed build Issue is one slice (section 5.3), names its declared test's claim, and its dependencies. The merge act's section 11 is the model; belt.md section 8 step 4 is the counter-example. |
| A build task's design, before its test commit | The worker; the design reader checks; the approver approves | The design has a section **Cut**: the concerns it found (5.3), and either "one slice" with the reason, or the split: which slice this task builds (its declared assertions), for each other slice a proposed Issue text (followup kind, `Follow-up of #N`), and the map of each item of #N's definition of done to the slice that carries it (5.4). The worker builds only its slice. |
| After the test commit, when the work grows | The worker | A review finding or CI failure that asks for a behaviour the declared test does not claim, and that the declared test does not need, goes out as a proposed follow-up in the done report, not into the diff. A defect of what the task adds is fixed in place. If the declared test itself needs a concern that should leave, the worker reports blocked with that judgment; the re-cut is then a new task with its own test commit (abandon and rebuild), which the secretary decides. |
| Out of review rounds (`REVIEW_ROUND_DENIED`) | The worker | Already in the rules: "a design problem, or separate issues surfacing one after another". The report names the concerns of 5.3 the rounds touched, so the secretary can file them. |

No moment depends on the person's turn except where the design is already the person's (G2).

### 5.3 The signals of a split

A signal is a judgment the designer writes down and the design reader checks; none is a count.

1. **Two behaviours that are each worth landing alone.** The Issue or the design names more than one behaviour a user of makiba would notice, and each could land and be useful without the other (Lawrence's business rule variations and workflow steps; Issue 284's two features).
2. **Assertion groups that fail for separate causes.** The declared test would hold groups of assertions that share no cause: one could fail while the other passes, for a reason in another module (Issue 475's YAML reader, readiness judgment and update act).
3. **More than one owner touched for separate duties.** The design extends two existing owners of two separate duties (a reader of a file format and an act that changes state; a record and a notice), not one owner and its callers.
4. **A dependency or a risk on part of the work.** Part of the work waits for another open task, or carries a risk another task closes, and the rest does not (Issue 284's ordering on #332). That part becomes its own slice and waits alone.
5. **A part that would edit what the gate protects.** Part of the work must change a test helper, `.github/`, the merge gate or the checker in a way the rest does not (helper-fix.md): that part lands by its own route (the lift, or the person), so it is a slice of its own and does not hold the rest.

And the counter-signals, which keep a cut from going too fine (section 3.4):

- A slice that cannot be shown by a declared test of its own, or that is useful only with the other, stays with it.
- A documents-only part that names the code's tests (lines.md rows, a skill's text that a test pins) goes with the code it describes, as merge-act's build Issue 6 shows; it may follow as its own slice only when it waits for several code slices.
- The concerns of section 5.3 are found in the code, not counted in the Issue's words: an Issue whose title joins two verbs may still be one concern.

### 5.4 The declared-test discipline under a split

- **Before the test commit** (the design moment), a split costs nothing of the discipline: each slice declares its own test when it is built; this task's test commit declares only its slice's assertions. Where the Issue's definition of done names one acceptance test for the whole, the design says which of its assertions this slice declares and which go to each proposed Issue; the secretary comments on the original Issue that its definition of done is now this slice's and names the follow-ups that carry the rest, and the slice's pull request closes the Issue as today. The Issue is not left open until every slice lands: the belt reads a dependency as met only when its Issue is `CLOSED` (`src/daemon/belt.rs`, the `depends on` read of the candidate scan), so a slice that depends on the first (`Depends on #N`) would otherwise wait for ever.
- **The dependents of #N.** Closing #N after its first slice would unblock, too early, every open Issue whose `Depends on` names #N for the whole of its scope: the belt reads only that line, and `Follow-up of #N` makes nothing wait. So the design's Cut section maps each item of #N's definition of done to the slice that carries it, and before the first slice's pull request lands the secretary finds every open Issue that depends on #N and rewrites each one's dependency line to the slices it needs, keeping #N only where the first slice is enough. To find them, a text search is not enough (it is not paginated, and a dependency line may name #N after other numbers, `Depends on #M #N`): the secretary lists every open Issue of the repository with pagination (`gh api --paginate "repos/<owner>/<name>/issues?state=open&per_page=100"`, pull requests left out) and reads every number on each line that starts with `Depends on`, `Blocked by` or `Requires`, the keywords the scan reads (`docs/design/spec.md` section 5.8). A dependent the secretary cannot judge goes to the designer of #N as a question before the landing.
- **Design-only runs already waiting for #N.** A rewritten dependency line does not reach a run dispatched for its design only: what it waits for is fixed at its dispatch (`belt_dispatched` `waits_for` and `waits_tasks`, read by `src/daemon/ahead.rs`), and it is woken when the task it waits for lands, that is when the first slice lands. The conservative rule, first: an Issue that an open design-only run waits for is not split, unless every such run needs only the first slice (its design says what it uses). Otherwise the work stays in one task, or the secretary first asks the person to close the waiting run as abandoned and dispatches its Issue again, one-off with `issue`, after the dependency lines are rewritten, so the new run waits for the slices it needs. The alternative, a code change, is build Issue 5: the wait follows the dependency lines as they are when it settles.
- **After the test commit**, the declared test cannot change (acceptance-check rules 2 and 4). Only work the declared test does not need may leave. Work it needs stays, or the task is rebuilt as a new task with a new test commit: that is the expensive path, which is why the cut is made at the design.
- A slice is never moved into `tests/common` or a helper to share it between slices: a helper change after a test commit is the case helper-fix.md is about.

### 5.5 How a slice reaches the belt

A slice is filed as a followup Issue (`Follow-up of #N`, the issue skill's kind followup, with `Needs host:` and `Stop line:` where they apply). The belt already takes a range's follow-ups by itself when the range's `follow_ups` is `inside` (the conveyor skill, belt.md section 11.3), so a split inside a range needs no new approval and no code: a slice of an approved Issue is the same work, cut finer, not a wider range. With `follow_ups` `ask` the slice is listed as asked for, as any follow-up is. A one-off task's slices are dispatched one-off by the secretary, each with `issue`.

### 5.6 What the design keeps

- **State.** None in memory, none in the ledger, no schema change: the cut is text in the design (the review's note), in the blocked report and in the Issues the secretary files. No door verb or tool is added, so `tests/door.rs` and `tests/door_tools_allowed.rs` are not touched.
- **Format.** Nothing parses a file format by hand for a safety judgment: the cut is a judgment read by a reader, not a parsed field.
- **The Issue and the code.** Where they disagree: Issue 284's test commit adds 725 lines in one test file, not two (`git show --stat 3d0f0bb`). The rest of the Issue's "What we know" was found as stated (the triage's "(estimated)" in `src/work_discovery/mod.rs`, the delegate skill's sentence); worker message 406 could not be read from here.

## 6. Options considered

- **A. The worker at design owns the cut, checked by the design reader, approved with the design (chosen).** From Scrum's "the Developers ... are responsible for the sizing", Shape Up's discovered scopes, ADaPT's as-needed decomposition and LLM-Modulo's verifier. Costs no new step and no state; uses the design round makiba already has.
- **B. The Issue writer owns the cut, at Issue writing.** Kept as part of A for decide designs (5.2, first row), but alone it is today's practice and made Issue 284: the writer cuts by the steps of a note, before the code is read.
- **C. The secretary at dispatch, as a refinement step.** The secretary has not read the code and would cut by the Issue's words; it adds a turn to every dispatch. Rejected; the secretary files and orders slices instead (the Product Owner's part).
- **D. A mechanical signal in makibad (lines, files, review rounds).** Rejected by the owner's direction and by section 3.2: size explains a part of the fixes (0.42), rebuilds do not follow it at all, and a threshold would split by lines, not concerns.
- **E. A person's step.** Against G2; the person approves a cut only where the design is already theirs.

## 7. Known limits

- Section 3 counts commits, not review rounds: a fix commit can answer several findings, and a round can find nothing. The per-task ledger table (review rounds, CI cycles, escalations) needs the person's export of `makiba events`; with it, section 3.2 can be redone, and the design reader's Major on 5.3 can be checked against what followed.
- The design reader runs with no network host (`review.rs`, `allowed_hosts` empty), so its check of this study covers the repository's records (sections 3 and 5) and the internal consistency of what sections 1 and 2 cite, not the outside sources themselves. Each outside claim names its source so that a reader with the network can check it.
- A judgment can be wrong both ways; the design reader is the check, and the follow-up route makes a missed split cheap (it leaves as a follow-up) only for work the declared test does not need.

## 8. Proposed build Issues (not filed until approved)

1. **The design reader checks the cut.** What: the design prompt of `src/daemon/review.rs` (`prompt(true)`) also asks whether the design carries more than one concern of section 5.3 without saying why they stay together, as a Major. Acceptance: a unit test in `review.rs` (or a test of the prompt text) that the design prompt names the cut and the diff prompt does not. Depends on: nothing.
2. **A design names its cut.** What: `roles/worker/rules.md`, "The review": a design has a section Cut (5.2, second row) with the proposed Issue texts of the slices that leave; after the test commit, a behaviour the declared test does not claim goes to the done report as a proposed follow-up (5.2, third row). `skills/delegate/SKILL.md`, the brief's item 7 points to it. Acceptance: a test in the style of `tests/sync_turn_report.rs` that the worker rules carry the section's sentences. Depends on: nothing.
3. **The secretary files and orders slices.** What: `skills/issue/SKILL.md` (a build Issue proposed by a design is one slice and names its declared test's claim; a slice of #N is a followup with `Follow-up of #N`, and #N gets a comment narrowing its definition of done to the first slice, which closes it; before that slice lands, every open Issue that depends on #N has its dependency line rewritten to the slices it needs, from the Cut section's map, section 5.4); `skills/conveyor/SKILL.md` section 2 (a design's approved split is filed at once, and the belt takes it with `follow_ups` `inside`); `roles/secretary/rules.md`, "Delegate" (the secretary does not cut; it approves the cut with the design). Acceptance: `tests/secretary_skills.rs` and `tests/conveyor_skill.rs` gain assertions for the sentences. Depends on: 2.
4. **The per-task record, when the export exists.** What: redo section 3.2 from a person's export of `makiba events` (review rounds, CI cycles, escalations per task) and add it here. A document task; needs the person's export. Depends on: nothing.
5. **(Optional, instead of the conservative rule of 5.4) A design-only wait follows a split.** What: `src/daemon/ahead.rs` `settle`, before it settles a wait as landed, reads the current dependency lines of the waiting run's Issue (from the range read that `belt.rs` already makes) and adds to the wait any open Issue they name that is a follow-up of an Issue it waits for. Acceptance: a regression test in the style of `tests/belt_ahead.rs`: a design-only run waits for #N; #N is split into #N and a follow-up #M, the waiting Issue's dependency line is rewritten to `Depends on #M`; the landing of #N's first slice does not wake the run, and the landing of #M does. Depends on: nothing. Changes the spec's design-ahead paragraph (`docs/design/spec.md` section 5.8).

Files these Issues change: `src/daemon/review.rs`, `roles/worker/rules.md`, `roles/secretary/rules.md`, `skills/delegate/SKILL.md`, `skills/issue/SKILL.md`, `skills/conveyor/SKILL.md`, and tests (`tests/secretary_skills.rs`, `tests/conveyor_skill.rs` extended; one new test for the worker rules); Issue 5, if chosen, also `src/daemon/ahead.rs`, the design-ahead paragraph of `docs/design/spec.md` and a new belt test. None changes the spec's event table, a door verb, the merge gate, the checker or `.github/`.
