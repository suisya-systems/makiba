# Pull requests that conflict with each other wait for each other's landing (Issue 573): a design

Status: reviewed once as a design (round 1, reader codex: passed, 2 blockers, 4 majors, 1 minor,
each answered in this text; section 11); approved by the owner on 2026-11-27 with Q1 to Q4
answered as this note proposes (section 9), and built with it: the declared test is
`tests/queue_overlap.rs`. Goal clause G2: the flow of work does not wait for the person's turn.
Code is read at 2ff3983 (origin/main when this was written; the brief's 4348262 is older). No
GitHub host was reachable; a claim about GitHub is marked [unverified].

## 1. What happens today

PR 520 (Issue 262) was taken out of the merge queue three times on 2026-11-26/27: each time a pull
request queued before it had changed the same lines of a shared document (docs/design/spec.md,
lines.md, probes.md). The queue tests an entry on top of every entry before it [unverified, as
GitHub documents it]; two entries whose own changes conflict cannot both be in it, so the later one
is dropped (`UNMERGEABLE`). PRs 530, 519, 565 and 572 met the same files.

makibad today, for a pull request it follows (`prwatch::followed`):

- `queue_readiness` (docs/design/queue-ready.md, Issue 475) tells a head "can be queued" when its
  CI passed, its merge state is not `DIRTY` and no landing since the base it holds changed what the
  checks check. It judges each pull request **alone against its base**: two heads that each merge
  cleanly with main and conflict with each other are both told "can be queued".
- When the earlier one lands, the later head's merge state becomes `DIRTY` and readiness starts the
  `update` job (queue-ready.md section 3.3, the conflict row). The job merges with
  `git merge-tree --write-tree` (`push.rs` `merge`); where the two changes touch the same lines it
  fails, naming the files, and the secretary has the worker merge.
- The queue's drop of the later one is told by the PR watch (`merge_dequeued`, Issue 476).

So what is lost today is the later pull request's queue entry, its drop, and the person's or the
secretary's queue request again. A conflict of the same lines always needs a worker's merge: git
cannot resolve it, and makibad does not edit a pull request's content.

## 2. Who owns it

The PR watch's readiness judgement, the owner queue-ready.md section 2 names: `PrWatch::readiness`,
`ready_candidates` and `decide` in `src/daemon/prwatch.rs`, with the git reading in
`src/daemon/ready.rs`. This change adds no thread, no table, no event kind and no second watcher:
it adds one reading (the overlap) to the reader that already fetches a candidate's head, one
verdict (`hold`) to the event that already records the judgement, and one input (a peer's
`pr_merged`) to the pass that already lists the candidates. Queue membership stays the PR watch's
(`Followed.act`: a queue act of makibad or an open hand stay, Issue 476).

How Issues 567 and 575 fit the same owner: no design note of either is on origin/main or on a
local branch, so this is read from the brief. Issue 567 changes the judgement's rule (PR 534
changed `scripts/acceptance-check.sh`, which `ready.rs` `CHECKED_PATHS` does not list): that is the
path rule of `ready.rs` section 4 of queue-ready.md. Issue 575 picks up a queue entry the queue
dropped as `UNMERGEABLE`: that is an input from the PR watch's queue reader (`merge_dequeued`) to
the same `decide`. All three write `queue_readiness` through `decide` and none adds a second
judge. With this change a followed pull request is no longer told ready behind a conflicting one,
so 575's case narrows to pull requests queued against a hold or not followed. The three branches
touch the same functions of `prwatch.rs` and `ready.rs`: by the rule of this note they land one
after another.

## 3. The rule

### 3.1 Which overlap counts (Q1)

Two pull requests **conflict** when `git merge-tree --write-tree --name-only --no-messages <their
head> <our head>` exits 1 (the files are the lines after the tree). Same lines, or lines next to
each other, conflict (git's three-way merge); changes elsewhere in the same file do not.

A same-file overlap that merges cleanly is not counted: the queue merges it and runs CI on the
combination (queue-ready.md section 1: "the queue tests the combination"), so holding it would only
make the later one wait with no entry saved. The reading is of the two heads against their own
merge base, not of the later head on top of the base with the earlier one: it is the conflict of
the two pull requests' own changes, which is the case of PR 520. Known limit: a conflict that only
arises with a landing in between (both heads older than main) is not seen, and is the existing
`DIRTY` path's.

### 3.2 The peers of a candidate

For a readiness candidate X (queue-ready.md section 3.1, extended in 3.3 below) of repository R, its
**peers** are the followed pull requests O of R (repository in any case), O's number not X's, not
read closed, at O's newest recorded head (`newest_head`), that are **before X in the queue's
order**:

1. O is in the queue (`Followed.act`: a queue act of makibad, or an open hand stay: the person
   queued it in the web UI), whatever X is; or
2. O's newest `queue_readiness` of that head is `ready`, and X has no `ready` of its head, or X's
   newest `ready` has a larger event id than O's (X was told ready after O).

Rule 1 wins over everything else: a pull request in the queue is a peer whatever its own newest
verdict (`hold` included: a held pull request the person queues by hand holds the others). Out of
the queue, a pull request held (`hold`), updating, or judged `update` or `unknown` is nobody's peer.

**A candidate in the queue gets no peers.** `ready_candidates` excludes a makibad act
(`door::act_awaiting`) but not an open hand stay, so a pull request the person queued is still a
candidate today. This change gives a candidate with `Followed.act` true (an open hand stay) no
peers, so it is never held; `Followed` (with `act`) is part of `ReadKey`, so a stay that opens
while a reader reads changes the key and `decide` drops the reading. Whatever else readiness does
to a hand-queued head today (an update on `DIRTY` or on a landing that counts) is not changed here
(a proposed follow-up, section 10). A peer is
listed from the ledger in `ready_candidates`, with its task, number, branch and head, sorted by
number, and is part of the candidate's `ReadKey`: a peer that comes, goes, or changes head makes
the running reader not wanted (it is stopped and started again), and makes a reading that comes
back after the change not written (`decide` compares the key again inside its transaction, as it
does today). So a pull request told ready or queued while a reader read is never missed.

### 3.3 The reading and the decision

The reader of X (`ready::read`, extended; one fetch as today) also fetches each peer's branch
(`+refs/heads/<O branch>:refs/makiba/ready/<X task>/peer/<O pr>`, in the same `git fetch`), and a
peer whose branch at origin is not at its recorded head makes the reading `stale` (nothing written,
read again next interval). For each peer it runs the merge-tree of 3.1. The reading gains
`overlaps`: each conflicting peer with its files.

The overlap is read on **every reader's first reading**, whatever `covered` says: the `Unchanged`
shortcut of `ready::read` (the base tip equal to `covered`) skips the landings only, never the
peers. A peer change restarts the reader (its key changed), so a new peer with an unchanged base is
read at once (test 3). Later readings of the same reader do not read the overlap again: the peers'
heads are fixed by the key, and the merge-tree of two fixed commits does not change.

merge-tree tells a conflict by exit status 1 with the files on standard output; `ready.rs`
`Git::run` returns only git's message on a non-zero exit. The build adds `Git::run_status`, under
the same timeout, cancel and process group as `run`, returning the exit code and standard output:
exit 0 is no conflict, exit 1 a conflict with its files, anything else (or a stop) a failure of the
reading, which counts against the reader's budget as any git failure does.

The decision (added to queue-ready.md section 3.3's table, in this order: the rows above it win):

| Result | Verdict | Written |
| --- | --- | --- |
| merge class `conflict`, or a landing counts | `update` (as today) | as today |
| some overlap, X not held, or held with a different set of peers (number and head) | `hold`, reason `overlap` | the event, one notice |
| some overlap, X held with the same set | none | nothing |
| no overlap, nothing counts, X held or first judgement | `ready` | the event, one notice |
| no overlap, nothing counts, X watched | none (as today) | nothing |

`hold` is decided for a watched head too (X was told ready, and a peer came before it: the person
queued another pull request first). The event carries the new field `holds`: `[{"pr", "task",
"head", "queued": bool, "files"}]`.

### 3.4 A held head stays a candidate

Rule 5 of queue-ready.md section 3.1 becomes: the head has no `queue_readiness`, or its newest one
is `ready` (watched) or `hold` (**held**). A held head's reader **watches the base** as a watching
reader does (queue-ready.md section 3.2: each interval it fetches the base, an unchanged tip sends
nothing, a moved tip is read whole), with `covered` starting at the hold's `base_tip`: a landing
that counts makes the held head `update`, reason `checks_changed`, as for a ready head; a move that
counts nothing sends nothing, and the same hold is not written again. The cost is one fetch of one
branch per held head per interval, as for a ready head.

### 3.5 When the earlier one lands (the update)

Each pass, before readers are started, for each held candidate X: if a pull request named in X's
newest `hold` has a `pr_merged` (repository in any case, the same number) with an event id larger
than the hold's, the pass decides X at once, without a reader: verdict `update`, reason
`overlap_landed`, new field `after` `{"pr", "head", "merged": <pr_merged event id>}`, with the
`update` command (actor `makibad`, args with `readiness`), as the conflict row does today. The
`Push` pass runs it as any update: a merge, never a rebase or a force (`push.rs`). It reads the
ledger only, so a landing recorded while makibad was down is acted on after the restart.

What the update does then (Issue text and code disagree here, 3.7): the job merges main into X.
Where X conflicted with the landed one in the same lines, `merge-tree` conflicts again and the job
ends `failed` with the existing notice naming the files, and the secretary sends the worker to
merge (as today, `update-branch` step 5); the worker's `pr pushed` records the new head, which is
judged anew. Where it merges (the landed version no longer holds the conflicting lines: it was
changed in the queue or before it landed), `finish` records the new head and watches it
(queue-ready.md section 3.4), and its passed CI gets a first judgement.

A peer that leaves without landing (closed, dropped from the queue and not ready, its head moved)
changes X's key: X is read again, and is told ready if nothing else holds it.

### 3.6 Notices (to the secretary; in substance)

- hold: "<repo>#<pr> at <7> is held from the merge queue: it conflicts in <files, at most 3> with
  <repo>#<O> at <7>, which is <in the merge queue | ready for the queue> before it. makibad updates
  it when #<O> lands (docs/design/queue-overlap.md). <link>". For a watched head it begins
  "<repo>#<pr> at <7> is no longer ready for the queue: ". It never says "can be queued", which stays
  the one ready signal.
- the update on a landing: the existing update notice with the reason "<repo>#<O>, which it
  conflicted with in <files>, landed".

### 3.7 Where the Issue's text and the code disagree

- "after the first lands makibad updates the second and reports its new head": for an overlap of
  the same lines (3.1, the case of PR 520), the update cannot produce a head: git conflicts, and
  makibad does not resolve a conflict. makibad starts the update and its failure names the files;
  the new head comes from the worker's merge, and makibad reports it ready. The declared test
  (section 6) asserts that.
- "not 'can be queued' while an earlier followed pull request that conflicts with it is queued or
  ready": as written. "Earlier" is the queue's order as 3.2 defines it.

### 3.8 The merge verb

queue-ready.md section 6 made `merge_check` (`door.rs`, the merge gate's verb) refuse a head whose
newest `queue_readiness` is `update`. It also refuses `hold` (Q3), with the same message ("<repo>#<pr>
at <7> is not ready for the queue (makibad's queue_readiness <id>: overlap)"), so the secretary's
act cannot queue a held head. The person's queue in the web UI is not stopped by makibad.

## 4. Pull requests makibad does not follow, and the person's hand

- **Not followed** (opened outside makiba, or of a run closed more than a day ago): not a peer and
  not judged. A followed pull request queued after one of them can still be dropped; the existing
  paths tell it (`merge_dequeued`, then `DIRTY` and the update).
- **Queued by the person in the web UI**: an open hand stay makes it a peer at once (3.2 rule 1),
  whatever the order of the ready notices: a pull request told ready before it is told "no longer
  ready" and held. A held pull request the person queues anyway is in the queue (not a candidate);
  if the queue drops it, it is a candidate again, still held, and is updated when the earlier one
  lands.
- **Both in the queue**: makibad holds neither (a pull request in the queue gets no peers, 3.2); the
  queue drops one and the existing paths apply.
- A peer whose head moves (its own update) stops holding until its new head is ready again; then
  the order is the order of the new ready verdicts, and the earlier held one may come first. No
  two pull requests hold each other: out of the queue a held head is nobody's peer, and in the
  queue a pull request gets no peers, so a hold always runs from one that is not held, or is in
  the queue, to one that is neither.

## 5. State kept in memory

None beside the ledger beyond what queue-ready.md section 5 keeps: the readers' keys now carry the
peers. Ordering: the peers are listed from the ledger in `ready_candidates`, inside `decide`'s
transaction too. Restart: the readers are started again from the ledger; a held head is read again
once, and a landing of its peer recorded while makibad was down starts its update (3.5).

## 6. The declared test: `tests/queue_overlap.rs`

`Acceptance-Test: tests/queue_overlap.rs`. The world of `tests/queue_ready.rs` (makibad in the test
process on a state database of its own, `MAKIBA_PRWATCH_INTERVAL_MS` 50, a project repository with
a local bare `origin`, code runs whose worktrees are made with `run_repo::create`, pull requests
recorded with `prwatch::register`), with the `gh` stand-in extended as `tests/merge_queue_watch.rs`
does for the queue: the membership read (`gh api graphql` with `-F n<i>=`) answers
`isInMergeQueue` from a file per pull request, the per-pull-request GraphQL read answers `OPEN` or
`MERGED` (with its merge commit) from that file, `gh pr view` answers `OPEN`, the branch's head at
origin and `CLEAN`, and every CI read passed. All helpers are inside the file; the stand-in is
written through `tests/common/exe.rs`. Landings are commits pushed to origin's `main` from a clone.

1. `a_pull_request_that_conflicts_with_a_queued_one_is_held_and_updated_when_it_lands` (project
   `pa`; `docs/shared.md` with ten lines at M0; PR 1 of task `pa-one` changes line 3 to `one`, PR 2
   of task `pa-two` changes line 3 to `two`; both fork at M0):
   - PR 1 recorded: exactly one notice containing "o/pa#1 at <h1 7> can be queued"; then the
     stand-in reads PR 1 in the queue: one `merge_queue_entered` of PR 1.
   - PR 2 recorded at h2: exactly one `queue_readiness` of h2, `verdict` `hold`, `reason`
     `overlap`, `holds` exactly `[{"pr": 1, "task": "pa-one", "head": h1, "queued": true,
     "files": ["docs/shared.md"]}]`; exactly one notice containing "o/pa#2 at <h2 7> is held from
     the merge queue", "docs/shared.md" and "o/pa#1"; no notice containing "o/pa#2 at <h2 7> can
     be queued"; no `update` command of `pa-two`. After ten intervals more: still one
     `queue_readiness` of h2 and one such notice.
   - The secretary's `merge` request (`repo` `o/pa`, `pr` 2, `head` h2) through the door is
     refused with kind `conflict` and a message containing "not ready for the queue"; no
     `merge_attempted` of PR 2.
   - PR 1 lands: its merge commit (first parent M0, second h1) is pushed to origin's `main` and the
     stand-in reads PR 1 `MERGED` with that commit: one `pr_merged` of PR 1; then a
     `queue_readiness` of h2 with `verdict` `update`, `reason` `overlap_landed`, `after.pr` 1; one
     `update` command with actor `makibad`, `args.task` `pa-two`, `args.readiness` that event's
     id, ending `failed`; a notice containing "conflicts in docs/shared.md"; origin's branch of
     PR 2 still at h2.
   - The worker's merge: a merge commit of h2 and the new main whose `docs/shared.md` line 3 is
     `one two` is pushed to origin's branch, and `pr pushed` at h3 is recorded through the door as
     the worker: exactly one notice containing "o/pa#2 at <h3 7> can be queued" and a
     `queue_readiness` of h3 with `verdict` `ready` and `holds` empty or absent.
2. `a_pull_request_that_changes_other_lines_of_the_same_file_is_not_held` (project `pb`; PR 3 changes
   line 2, PR 4 line 9 of `docs/shared.md`): PR 3 told ready and read in the queue, then PR 4
   recorded: exactly one notice containing "o/pb#4 at <h4 7> can be queued", its `queue_readiness`
   `ready`; no `hold` of PR 4.
3. `a_head_told_ready_is_held_when_the_person_queues_a_conflicting_one_first` (project `pc`; PR 5 and
   PR 6 change line 3 differently): PR 5 recorded and told ready ("o/pc#5 at <h5 7> can be queued");
   PR 6 recorded: `hold` of h6 naming PR 5 with `queued` false. Then the stand-in reads PR 6 in the
   queue (the person queued it by hand): a `queue_readiness` of h5 with `verdict` `hold`, `watched`
   true, `holds` naming PR 6 with `queued` true, and a notice containing "o/pc#5 at <h5 7> is no
   longer ready for the queue"; still exactly one notice containing "o/pc#5 at <h5 7> can be
   queued". (A new conflicting peer with the base unchanged: the reading of h5 reads the overlap
   although its `covered` is the base tip.)
4. `a_held_head_is_updated_by_a_landing_that_changes_what_the_checks_check` (project `pd`; PR 7 and
   PR 8 change line 3 differently): PR 7 told ready, PR 8 recorded and held behind it. Then main
   lands L1, which adds `tools/check.py`, with no event in the ledger: a `queue_readiness` of h8
   with `verdict` `update`, `reason` `checks_changed` and `landings` naming L1 with
   `tools/check.py`; one `update` command with actor `makibad` and `args.task` `pd-eight`, ending
   `done`; a `fix_pushed` of PR 8's run whose `head_oid` is origin's branch, which holds L1.

The test ignores nothing and runs under `scripts/acceptance-check.sh` (it builds under the system
temporary folder, Issue 558).

## 7. Files the build changes

New: this note, `tests/queue_overlap.rs` (declared).

Changed (no existing test, no `.github/`, no script, no acceptance checker, no language check, no
schema change, no new event kind, verb, tool or status field):

- `src/daemon/ready.rs`: `Git::run_status` (3.3); `read_with_peers` (the peers' fetch in the one
  fetch, the merge-tree reading on a reader's first reading, `Reading::Read` gains `overlaps`); `read` stays as it is (it calls it with no peers), so
  its unit test does not change.
- `src/daemon/prwatch.rs`: `ReadyCand` and `ReadKey` gain the peers and the held mode (a held
  reader watches the base, 3.4; a candidate with an open hand stay gets no peers, 3.2);
  `ready_candidates` lists held heads and the peers; `readiness` decides a held head whose peer
  landed (3.5); `ready_done` and `decide` gain `Decision::Hold` and the `overlap_landed` update,
  the payload fields `holds` and `after`, and the notices of 3.6.
- `src/daemon/door.rs`: `merge_check` refuses `hold` as it refuses `update` (3.8; the merge gate's
  verb).
- `docs/design/spec.md`: section 5.6's queue-readiness paragraph (the held candidate, the peers,
  `hold`) and section 10's `queue_readiness` sentence (verdict `hold`, reasons `overlap` and
  `overlap_landed`, fields `holds` and `after`); section 12's `merge` row, which names `update` as
  the readiness verdict the merge verb refuses, names `hold` too. `queue_readiness` is kept out of section 10's
  table, so `src/spec.rs` and `tests/spec_matches_code.rs` do not change.
- `docs/design/queue-ready.md`: section 3.1 rule 5 and section 3.3 point to this note.
- `skills/pull-request/SKILL.md`, "A queue-readiness notice": the held notice (do not ask for the
  queue; wait for the earlier one's landing and the update's notice; on its conflict the worker
  merges). `tests/conveyor_skill.rs` reads the skill's stop lines, which do not change.

`tests/queue_ready.rs` and `tests/queue_ready_watch.rs` have one pull request per repository, so no
peer and no change of what they assert.

## 8. The shared documents (Definition of done 2)

Where the conflicts come from: docs/design/spec.md section 12's verb table (one line per verb, the
longest line over 11,000 characters) and its status field table, section 10's event table, lines.md's one
table (`| L-` rows) and probes.md's results table. git counts changes to lines next to each other
as a conflict, so two tasks editing neighbouring rows, or both adding a row at a table's end,
conflict although they touch different verbs or lines.

| Option | Conflicts left | What the tests need |
| --- | --- | --- |
| K. Keep the files; rely on this note's hold | every neighbouring edit still costs a worker's merge, but no queue entry | nothing |
| R. Keep the files; new rows go where their name sorts, never at the end | fewer adds at the end; edits of neighbouring rows still conflict | nothing (`tests/role_settings.rs` asserts L-K1..13, L-N1..9 in order; those ids already sort) |
| F. One file per row of the hot tables: `docs/design/spec/verbs/<verb>.md`, `spec/events/<kind>.md`, `spec/status/<field>.md`, `docs/design/lines/<id>.md`, `docs/design/probes/<id>.md`; the old section names the folder | only edits of the same verb, event, line or probe, which are real overlaps | `tests/spec_matches_code.rs` reads the verb and event facts from the folders (section 10's and 12's tables by `include_str!` today); `tests/read_surface.rs` reads the status shapes from `spec/status/` (`SPEC` by `include_str!` today); `tests/role_settings.rs` reads `docs/design/lines/L-*.md` (the expected list unchanged); each test edit is of an existing test |
| G. F's files plus a generator that writes the tables into spec.md, checked in CI | as K on the generated file whenever it is committed; none if it is not, but then no one file holds the spec | a new tool, a CI step under `.github/`, and the three tests read the generated file |
| U. `.gitattributes` `merge=union` on the three files | GitHub's merges do not run a repository's merge drivers [unverified]; locally, an edit of the same row by both keeps both rows | nothing, but duplicates pass unseen where a test does not compare sets |

Recommended: **F**, as a separate build Issue (it edits three existing tests and every document
that links into those tables, and it changes no behaviour): it removes the conflicts of different
rows, which are the cases of PR 520 (different verbs and lines in the same tables), and leaves only
the overlaps a reader must merge anyway, which this note's hold then orders. K alone keeps every
round's worker merge; R is cheap but does not help neighbouring edits; G and U add machinery with
the same or worse results.

## 9. Questions

- **Q1, the overlap.** A conflict of the two heads by `git merge-tree` (recommended, 3.1), or any
  file both change? The second also holds pull requests the queue would merge; its update after
  the landing then merges cleanly and makibad reports the new head itself.
- **Q2, the update after the landing.** For a same-lines conflict the update cannot make a head
  (3.7); the new head is the worker's merge. Is the declared test's step 1 (update `failed` naming
  the file, then the worker's head told ready) what the Issue's "makibad updates the second and
  reports its new head" means?
- **Q3, the merge verb.** `merge_check` refuses a held head (3.8), a change of the merge gate's
  verb. In this change, as queue-ready.md's Q3 was?
- **Q4, the documents.** F, as a separate build Issue (section 8)?

## 10. Proposed build Issues

- This note's build (sections 3 to 7), with the declared test of section 6.
- The shared documents by option F (section 8): spec.md's verb, event and status tables, lines.md
  and probes.md's results table into one file per row, and the three tests reading the folders.

Proposed follow-up, not in this change: readiness treats a pull request the person queued by hand
(an open hand stay) as a candidate, so it may update it while it is in the queue (on `DIRTY` or on a
landing that counts); whether it should be no candidate, as a makibad act is, is a question for
queue-ready.md.

## 11. The design review (round 1, reader codex: passed, 2 blockers, 4 majors, 1 minor)

The reader read a short form of this note (the file was not committed, so the round could not see
it; that was a major, answered by giving the owner the whole note).

- Blocker, "held pull requests are nobody's peer" contradicts test 3, where a held pull request
  queued by hand must hold: rule 1 (in the queue) now wins over every exclusion (3.2).
- Blocker, `ready_candidates` excludes makibad's acts but not hand stays, so a hand-queued head
  could be held: a candidate with an open hand stay gets no peers, checked again through the key in
  `decide` (3.2).
- Major, a restarted reader's `Unchanged` shortcut (base tip equal to `covered`) would skip a new
  peer: the overlap is read on every reader's first reading, whatever `covered` says (3.3; test 3).
- Major, `Git::run` drops standard output and the exit code on failure, so a merge-tree conflict
  cannot be told from an error: `Git::run_status` (3.3).
- Major, a held head read once per key misses a landing that counts: a held reader watches the
  base as a ready one does (3.4; test 4).
- Minor, section 12's `merge` row: named in section 7.
