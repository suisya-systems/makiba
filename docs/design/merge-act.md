# makibad runs the merge the gate allows (Issue 425): a design

Status: design, for review. No code, tests, CI or settings change with it. The owner decided D1-D4
on 2026-11-24 (Issue 425); this note designs them and does not reopen them. The changes are
proposed as build Issues in section 11.

Code is cited at b21b150. A claim about GitHub or gh is marked **verified** with its source, or
**[unverified]**; the unverified ones are listed in section 10. No GitHub host was reachable while
this note was written (github.com and its subdomains are outside the network allowed here), so no
GitHub document was read for it.

## 1. The decisions

- **D1, option A.** When the merge gate allows a pull request, makibad itself runs the landing (a
  merge, or the enqueue on a branch that requires GitHub's merge queue) in the same act that allows
  it. The secretary only asks, with the door verb `merge`. Option C (an identity only makibad
  holds) is a later, separate decision; nothing here depends on it, and the verb stays the same if
  it comes.
- **D2.** Under `merge_approval_mode` `human`, the secretary's request becomes a pending entry, and
  the person approves it with their own key (`makiba merge approve <id>`, as `makiba range
  approve`; later a button on the screen of Issues 169 / 404). The person's word in the chat no
  longer lets a merge run.
- **D3.** With the merge queue, the enqueue is the act makibad runs and records; the landing that
  follows is read by the PR watch. This answers merge-wait.md's Q3: the person's word under `human`
  is given for the enqueue of one head.
- **D4.** A failed or unknown act is never retried automatically. It is recorded, the secretary is
  told, and the secretary asks again (a new `merge` call, judged again by the gate).
- **The restart criterion** (Issue 425, comment of 2026-11-24): after a restart, an attempt with no
  recorded outcome whose pull request GitHub shows merged at the expected head is recorded as
  observed, with the actor unknown. Section 4.4 generalizes it to every outcome that is read
  rather than answered.

What this fixes:

1. The secretary's own permission layer can no longer refuse a merge the gate allowed after the
   allowance was recorded (Issue 392: the gate allowed head 7fcbd51, the secretary's permission
   check then refused the command). The gate now runs inside the verb, so an allowance and the act
   are one record. The secretary's permission layer can still refuse or ask about the call of the
   tool `mcp__makiba__merge` itself; that refuses a request before any allowance exists, which is
   harmless, and the build adds the tool to the secretary's allow rules (section 9) so it is not
   asked.
2. An allowance can no longer stand for a merge that never ran (Issue 424's cause): the PR watch
   classes a merge from makibad's act records only, never from an `acceptance_gate` row.

## 2. Today, read from the code

- The hook `makiba hook merge-gate` lets exactly one form run, `gh pr merge <n> --repo <o>/<r>
  --squash|--merge|--rebase --match-head-commit <sha> [--delete-branch]` alone on the line
  (`src/hooks/merge_gate.rs:89-115`), after asking the door's `gate` verb with the gate key
  (`merge_gate.rs:737-773`). It refuses every other merge form it reads (`merge_gate.rs:20-29`,
  `refused_gh` at `:631-709`). Its GraphQL check is `merge_word` (`:117-133`), which catches
  `mergePullRequest`, `automerge` and `mergeBranch` but not `enqueuePullRequest`.
- Only the gate key or the person may call `gate` (`src/daemon/door.rs:275-290`; the secretary's
  verbs, `src/spec.rs:285-288`, have no `Gate`). `gate` reads GitHub and git with the door's lock
  released (`gate_outside`, `door.rs:2990-3077`), then judges and commits `acceptance_gate` with
  `allow` in one transaction (`door.rs:3082-3341`, the event at `:3256-3262`). Under `human`, or on
  a range-mode mismatch, a warn case keeps `allow` with `warn` true (`door.rs:3150-3221`), and the
  hook turns it into permission "ask" (`merge_gate.rs:711-724`). A clean pull request under
  `human` is allowed with no question; the person's word is then held only by
  `roles/secretary/rules.md:54` and `skills/pull-request/SKILL.md` ("Merging").
- The PR watch classes a merge `merged_via` `gate` when any `acceptance_gate` allow exists for the
  pull request at `headRefOid`, at any time (`src/daemon/prwatch.rs:1049-1060`), and tells the
  secretary only of a `hand` merge (`prwatch.rs:1079-1094`). Its state read is one `gh pr view`
  with `state,mergedAt,mergedBy,headRefOid,mergeCommit,title,mergeable,mergeStateStatus,isDraft`
  (`prwatch.rs:814-815`).
- `push` and `update` answer `accepted`; `src/daemon/push.rs` gives each `accepted` command a
  thread, writes the outcome with a notice on a later tick, and **runs an `accepted` push again
  after a restart** (`push.rs:13-14`, `:610-686`). The merge act must not copy that last rule (D4).
- makibad's gh runs with makibad's environment and the person's stored login
  (`src/daemon/gh.rs:17-22`, `:59-65`); gh's stderr is discarded (`gh.rs:65`), so a failure reads
  only "exited with <status>" (`gh.rs:117-118`).
- The `pending` table's `kind` is `question` or `prompt` (`src/db/schema_v4.sql:61-75`).
- `makiba range approve` prints the draft and asks y/N in the person's shell before it sends the
  person-keyed verb (`src/cli.rs:684-779`); the door refuses `approve` from any role but the
  person (`src/daemon/belt.rs:186-194`).

## 3. The verb `merge`

One door verb, `merge`, with an MCP tool for the secretary. `Verb::Merge` is added to
`src/spec.rs` (the enum, `ALL`, `as_str`) and to `Role::Secretary`'s verbs; the person has every
verb. Workers and the curator do not get it.

| `action` | Arguments | Who | What it does |
| --- | --- | --- | --- |
| `request` (default) | `repo` (`owner/name`), `pr`, `head` (40 lowercase hex), `method` (`squash`, `merge` or `rebase`; default `squash`) | secretary, person | Runs the gate at `head` (section 3.1). Refused: answers the gate's verdict, as `gate` does today; nothing else. Allowed with no warning under `acceptance`, or by the person under either mode: commits the attempt and answers `accepted` (section 4). Allowed under `human` when the caller is the secretary, or with a warning: writes a pending entry and answers `pending` with its id (section 5). |
| `approve` | `id` (the entry), `shown` (the entry's `text_sum` the CLI printed) | person only | Section 5.2. |
| `refuse` | `id`, optional `reason` | person only | The entry `answered` with `refused`; the secretary is told. |
| `withdraw` | `id`, optional `reason` | secretary, person | The entry `withdrawn`. Narrowing, so the secretary may. |

The `gate` verb stays, for the person only, as a judgment that runs nothing (it still writes its
`acceptance_gate` row). Since the PR watch no longer reads that row as a merge's record (section
7), an allowance without an act no longer misleads anything.

### 3.1 The gate inside the verb

`Door::gate` (`door.rs:3082-3341`) is split: the reading and judging part, up to and including the
`acceptance_gate` event, the belt's stop and `gate_allowed`, becomes one function both verbs call;
`gate` then records its answer, and `merge` goes on, in the same transaction, to write either the
attempt (section 4) or the pending entry (section 5). So the allowance, the stop and the attempt
commit together or not at all. The gate's checks, warnings and verdicts do not change. The
`acceptance_gate` payload gains `"via": "merge"` or `"via": "gate"` so a reader can tell which
verb judged.

Before the attempt or the entry is written, in the same transaction, the verb refuses with
`conflict` when the pull request already has an open attempt (an `merge_attempted` with no outcome
event, section 4.2) or an open pending merge entry. That check runs after the lock is taken again,
so two calls that ran their gates at once cannot both write one. A call with the same `idem` is
answered from `commands` as every verb is (`replay`, `door.rs:588`).

### 3.2 Answers

- Refused: `{"status": "refused", "verdict", "reason", "mode", "repo", "pr", "head", "review"?}`;
  the command `rejected`, as a refused `gate` is today.
- Attempt: `{"status": "accepted", "attempt": <event id>, "repo", "pr", "head", "method", "mode"}`;
  the command stays `accepted` until the outcome finishes it (`done` or `failed`), as `push`.
- Pending: `{"status": "pending", "pending_id", "repo", "pr", "head", "mode", "warnings": [...]}`;
  the command `done`. The secretary tells the person the entry's id and the command to approve it,
  and ends its turn.

## 4. The act and its records

### 4.1 Records (events in the ledger)

| Event | Written by | When | Payload |
| --- | --- | --- | --- |
| `merge_attempted` | the door, in the verb's transaction | before anything is sent to GitHub | `repo`, `pr`, `head`, `method`, `gate_event` (the `acceptance_gate` id), `command` (the `commands` id), `pending_id` (when approved from an entry), `approved_by` (`person` or none), `boot` (section 4.3), `task_id` |
| `merge_done` | the act's thread's outcome, written by the loop | GitHub answered that the pull request merged | `attempt`, `merge_commit`, `answer` (GitHub's message) |
| `merge_enqueued` | same | GitHub answered that the pull request is in the queue | `attempt`, `entry` (the queue entry's id and position, when read) |
| `merge_failed` | same | GitHub answered a refusal, or makibad found before sending that it cannot act | `attempt`, `sent` (bool), `exit`, `http_status` (when read), `stderr` (bounded, section 8), `state` (the pull request's state read right after) |
| `merge_unknown` | same, or the restart reconciliation | the answer could not be read (a timeout, a killed or crashed call, unreadable output), or makibad restarted with the attempt open | `attempt`, `observed` (`merged`, `merged_other_head`, `enqueued`, `open`, `closed` or `unreadable`), `actor` (`unknown`), `state` (the read), `why` |
| `merge_dequeued` | the PR watch | a pull request makibad enqueued left the queue without landing | `repo`, `pr`, `head`, `attempt`, `state` (the read) |

Each outcome event is written with the attempt's command finished (`done` for `merge_done` and
`merge_enqueued`, `failed` for the others) and one notice to the secretary, in one transaction, as
`push.rs`'s `finish` does (`push.rs:700-760`).

Notices (the text the build writes, in substance):

- done: "makibad merged <repo>#<pr> at <head> (merge commit <short>). The PR watch records the
  landing."
- enqueued: "makibad added <repo>#<pr> at <head> to the merge queue. The PR watch tells you when it
  lands or leaves the queue."
- failed: "makibad's merge of <repo>#<pr> at <head> was refused by GitHub: <message>. Nothing was
  retried. The pull request is <state>. Deal with the cause, then ask again with merge."
- unknown: "makibad cannot tell whether its merge of <repo>#<pr> at <head> ran (<why>); GitHub
  shows it <observed>. Nothing was retried. If it is open, ask again with merge; if it landed, the
  PR watch records the landing with the actor unknown."

### 4.2 The act

A new pass, `src/daemon/merge.rs`, run by makibad's loop beside `push.pass`
(`src/daemon/mod.rs:360`, `:411`). It owns the act and nothing else; it neither judges nor watches.

An attempt is **open** while it has none of `merge_done`, `merge_enqueued`, `merge_failed` and
`merge_unknown`. For each open attempt the pass either runs it (once) or reconciles it (section
4.4). The thread runs gh outside any transaction and touches no database, as push's threads do:

1. Read the pull request with one GraphQL query through gh: its node id, `state`, `headRefOid`,
   whether its base branch requires the merge queue, and whether it is in the queue
   [unverified: the fields `isMergeQueueEnabled`, `isInMergeQueue` and `mergeQueueEntry` on
   `PullRequest`]. Not `OPEN`, a head other than the attempt's, or a read that cannot tell whether
   the queue is required: `merge_failed` with `sent` false. Nothing was sent, so this is certain.
2. No queue: the REST merge, `gh api -X PUT repos/<o>/<r>/pulls/<n>/merge -f sha=<head> -f
   merge_method=<method>` [unverified: that GitHub refuses the call when `sha` is not the head, and
   answers `merged` and the merge commit's `sha` on success; source to read:
   https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request]. Not `gh pr merge`: its
   behaviour on a queue branch and its handling of `--match-head-commit` there are
   [unverified] (merge-wait.md section 5, claim 5), and the REST answer is JSON makibad can read.
   `--delete-branch` is not carried: the run's branch stays until the task's close.
3. Queue required: the GraphQL mutation `enqueuePullRequest(input: {pullRequestId, expectedHeadOid:
   <head>})` [unverified: the mutation's name and its `expectedHeadOid`; source to read:
   https://docs.github.com/en/graphql/reference/mutations#enqueuepullrequest]. `method` is
   recorded but not sent: the queue's merge method is the branch's setting [unverified].
4. The outcome: a readable success is `merge_done` or `merge_enqueued`; a readable refusal from
   GitHub (an HTTP status and a message, or a GraphQL error) is `merge_failed` with `sent` true;
   anything else (a timeout, a kill, an exit with nothing readable) is followed by the read of step
   1 again and recorded `merge_unknown` with what it observed. A failure is read again for the
   state, never sent again.

The call uses `gh.rs`'s `run_bounded` with its timeout (120 s, `gh.rs:12`), so no attempt stays
open for ever in a running makibad.

### 4.3 What is kept in memory, and its rules

The ledger is the only record. The pass keeps two things in memory, both rebuilt or emptied at a
restart, neither of which decides anything a restart could repeat:

- **`boot`**: an id makibad draws at start (random, 128 bits) and hands the door before the door
  serves its first request. The door writes it into each `merge_attempted`. The pass **runs** only
  an open attempt whose `boot` is its own; an open attempt with another `boot` was written by an
  earlier makibad and is **reconciled** (section 4.4), never run. This is what keeps D4 across a
  restart, where `push.rs` reruns.
- **`running`**: the attempt ids with a thread, and the pull requests (`repo`, `pr`) with one, as
  `Push`'s `running` and `busy`. An attempt in it is skipped; a finished thread's outcome comes back
  on a channel and is written on the loop's thread, and a write that fails is tried again on the
  next tick (as `push.rs:611-629`).

Ordering:

1. `merge_attempted` is committed, with the `acceptance_gate` row and the command, before the pass
   can see it. Nothing is sent to GitHub before that commit.
2. The thread starts on a later tick and sends at most one request (step 2 or 3).
3. Its outcome event, the command's end and the notice are one transaction.

A crash between 1 and 2 leaves an attempt that never sent; a crash between 2 and 3 one that may
have. A restarted makibad cannot tell them apart, so both are reconciled the same way, and neither
is sent again.

### 4.4 Restart reconciliation, and outcomes that are read

The pass reconciles each open attempt of another `boot` by reading the pull request (the read of
step 1, plus `mergeCommit` and `mergedAt`), and records `merge_unknown`, with `actor` `unknown` and
`observed`:

| GitHub shows | `observed` | Then |
| --- | --- | --- |
| merged, `headRefOid` the attempt's head | `merged` | The comment's criterion. The PR watch records the landing with `merged_via` `unknown` (section 7). makibad and the person merge with one login (section 6), so the state alone cannot say who merged. |
| merged at another head | `merged_other_head` | Not this attempt's (the head is bound by `sha` or `expectedHeadOid`); the PR watch classes it `hand`. |
| in the queue at the head | `enqueued` | The PR watch follows the queue as for `merge_enqueued`; a landing is `unknown`, not `gate`. |
| open, not queued | `open` | Not landed. The secretary asks again (D4). |
| closed | `closed` | Nothing to do. |
| cannot be read | (none yet) | Read again each tick for up to the PR watch's budget (300 s, `prwatch.rs:49`), then `unreadable`. Reading again is not acting again. |

The same table settles an in-process call whose answer could not be read (step 4 of 4.2): an
outcome that is read rather than answered is `merge_unknown`, never `merge_done`, even when the
pull request shows merged at the head seconds later. This applies the owner's restart criterion to
every case where the same login leaves the actor unproven; the Issue's sketch ("timeout after
sending: state read") is read that way.

## 5. Under `human`: the pending merge entry

### 5.1 The entry

A `pending` row of a new kind, `merge` (a schema step widens the `kind` check of
`schema_v4.sql:65`), on the task's run when the pull request has one (the newest `pr_watches` row
names it), else with no run. `question`: "Merge <repo>#<pr> at <short head> (<title>)?".
`context_json`: `repo`, `pr`, `head`, `method`, `mode`, `gate_event`, the gate's `reason`, its
`warnings` (each warn case of `door.rs:3150-3221` as its own line: review state, edited tests,
range mode), `requested_by`, and `text_sum`, the SHA-256 of the text the CLI prints. It shows in
`makiba status` and among the pending entries as any other; the screen of 169 / 404 later shows it
with an approve button that sends the same verb with the person key.

The `answer` verb refuses an entry of kind `merge` (`forbidden`: "approve it with makiba merge
approve <id>"), so the secretary cannot answer it. A second `request` for the same pull request and
head while the entry is open answers that entry again (as `pending add` does, `door.rs:895-908`).

The entry is withdrawn by makibad, with a notice, when its head stops being the pull request's: a
`pr pushed` of another head (the door's `pr` verb), or a `pr_merged` or `pr_closed` the PR watch
writes.

### 5.2 Approval

`makiba merge approve <id> [--yes]` in the person's shell, as `range approve`: the CLI reads the
entry from `state.db` as `status` reads it, prints the pull request, head, title, the gate's reason
and each warning, and makibad's `merge_approval_mode`, asks y/N unless `--yes`, and sends `merge`
`approve` with `id` and `shown` (the `text_sum` it printed) over the door with the person key.

The door, for the person only:

1. The entry must be `open` and its `text_sum` equal to `shown`; else `stale` (the entry changed
   since it was printed).
2. It runs the gate again at the entry's head (section 3.1, the lock released as for `gate`).
3. Refused: the entry stays open, the answer carries the verdict, and the secretary is told.
   Allowed with exactly the entry's warnings, or fewer: the entry `answered` (`approved`,
   `answered_by` `person`) and `merge_attempted` with `approved_by` `person` and `pending_id`, in
   one transaction; the act follows as in section 4. Allowed with a warning the entry did not
   carry: the entry's context and `text_sum` are replaced with the new reading and the answer is
   `stale`, so the person approves what they were shown and nothing else.

The person's direct `request` with no warning acts at once (their key is the word); with a warning
it writes an entry too, so a warning is always printed before the person's key lets it land.

## 6. Credentials, and what GitHub can tell

makibad runs gh with its own environment and the person's stored login (`gh.rs:17-22`), the same
login the secretary is given (`src/daemon/launch.rs:73`, `:296-310`). So GitHub's `mergedBy` names
the person's account whoever merged, and cannot tell a makibad merge from the person's hand merge
in the web UI. makibad's own records carry that distinction; Option C would move it to GitHub. What
`mergedBy` reads after a queue merge is [unverified] (merge-wait.md section 5, claim 6).

The secretary keeps its gh login: it still opens pull requests, reads them and reruns CI jobs. The
bound against merging around makibad stays GitHub's branch protection, set by the person; the hook
(section 8) is the second layer, as today.

## 7. The PR watch: classification and notices

`prwatch.rs` stays the only watcher. Changes to `apply_state` (`prwatch.rs:1026-1110`):

- **Classification.** On `MERGED`, `merged_via` is read from the act records of that pull request
  at `headRefOid`, newest first, ignoring any `acceptance_gate` row:
  - `gate`: a `merge_done`, or a `merge_enqueued` with no `merge_dequeued` after it.
  - `unknown`: a `merge_unknown` (any `observed` but `merged_other_head`), or a `merge_enqueued`
    followed by a `merge_dequeued` and no later act (the person may have enqueued it again by
    hand).
  - `hand`: none of these.
  `pr_merged` gains `attempt` (the act's event id) when there is one.
- **Ordering with the act.** While the pull request has an open attempt (section 4.2), a `MERGED`
  read is not written: the read is dropped and the poller reads again on its next interval. The
  attempt gets its outcome within the call's timeout, or at a restart from the reconciliation, so
  the wait ends. This keeps a merge makibad's own thread made from being written `hand` because
  the watch read it first.
- **Notices.** Issue 424's ask 1 (tell the secretary of every merge, whatever made it) is built by
  424, first and independently. On top of it, the notice names `merged_via`: for `gate`, "landed
  (makibad's merge)"; for `unknown`, "landed; makibad cannot tell whether its attempt or another
  merge landed it"; for `hand`, as today (`prwatch.rs:1085-1092`). Each ends with the retro and
  the close, as today.
- **The queue.** The state read gains the queue entry of the pull request [unverified: whether `gh
  pr view --json` has a field for it; if not, the state read becomes one `gh api graphql` query
  with the same fields, still one call per interval, no second poller]. When the newest act record
  of the pull request at its head is `merge_enqueued` (or `merge_unknown` with `observed`
  `enqueued`), and a read shows it `OPEN` with no queue entry, the poller reads once more on its
  next interval; when that read shows the same, `merge_dequeued` is written and the secretary told:
  "<repo>#<pr> left the merge queue without landing (the queue's checks failed, or someone removed
  it). Read why on the pull request; fix it or ask again with merge." The second read guards a
  pull request read between leaving the queue and showing `MERGED` [unverified: whether GitHub
  shows such a state]; the first read is kept in the poller's memory only, so a restart costs one
  more interval, nothing else. The reason for the removal is not read [unverified: GitHub's
  timeline has a removal event with a reason].

`followed` (`prwatch.rs:837`) needs no change: an enqueued pull request has no `pr_merged`, so it is
followed until it lands, closes or is dequeued.

## 8. The hook with no allowed form, and gh's stderr

`src/hooks/merge_gate.rs`:

- `allowed_form` goes; `parse` returns a refusal or `None`. Every `gh pr merge` is refused, with:
  "a merge goes through makibad: call mcp__makiba__merge with repo, pr and head (the full head of
  the CI notice); under merge_approval_mode human the person approves it with makiba merge approve
  <id>".
- `merge_word` adds `enqueuepullrequest`, so `gh api graphql` with the enqueue mutation is refused
  as a merge mutation is. `dequeuePullRequest` holds no merge word and stays allowed: taking a pull
  request out of the queue only narrows.
- `ask_gate` and `gate_answer` go: the hook asks nothing and needs no door, so a hook that cannot
  read its input still refuses (L-K11), and it refuses the same with makibad down.
- The other refusals stay (`gh api` writing to `pulls/<n>/merge`, `curl` or `wget` to the GitHub
  API, the wrappers and runners). The hook still stops mistakes, not an adversary; the bound is
  branch protection.

The gate key is retired with it: the door no longer accepts it (`is_gate_key`, `door.rs:247-253`,
`:275-291`), makibad stops writing it (`door.rs:3877`, `src/daemon/person_key.rs`), and the
secretary's launch stops naming it (`launch.rs:316-319`). The deny rules and the redactions that name
`gate.key` (`src/status_local.rs:551`, `src/daemon/prepare.rs:365`, `src/daemon/review.rs:767`,
`:936`) stay, since an old file may still be on disk.

`src/daemon/gh.rs`: `run_bounded` keeps stderr (piped and read on its own thread like stdout, the
first 4 KiB kept), and `GhError::Failed` carries it beside the exit status. The existing callers
keep their messages and may append it. The act needs it to tell GitHub's refusal from a failure to
answer; gh writes an API error's message to stderr and exits non-zero [unverified].

## 9. Texts that change (the build writes them; this note does not edit those files)

**lines.md L-K2**, the line: "Only the secretary and makibad write to GitHub." Added to its
holders: "makibad writes to GitHub only to push the run's branch, never forced (`tests/push_verb.rs`),
and to land or enqueue a pull request in the act its gate allowed (`tests/merge_act.rs`); the
secretary does not merge (`hook merge-gate`)." The rest of the row stays.

**lines.md L-K9.** PR 402 (Issue 401) rewrites this row too; the build applies this on top of
whatever has landed. Its holder part becomes: "makibad, outside the agent: the door's verb `merge`
runs the acceptance gate and, only on its allowance, lands the pull request (or enqueues it) bound
to the judged head, in one recorded act (`merge_attempted` before anything is sent; never retried),
`tests/merge_act.rs::an_allowed_merge_is_run_by_makibad_at_the_judged_head`,
`tests/merge_act.rs::a_refused_merge_sends_nothing`,
`tests/merge_act.rs::an_open_attempt_is_reconciled_not_run_after_a_restart`. Second layer: `hook
merge-gate` refuses every merge and enqueue from the secretary's command line, with no allowed
form. Not held: a merge in the GitHub web UI by the person (held by branch protection, which the
person sets)."

**lines.md L-K10**, in its merge-gate sentence: "...and, under human, allowed with a warning that
makibad writes into a pending merge entry the person approves with their key (`makiba merge
approve`), except a head the last diff review did not read...". The test names that name the hook's
question (`tests/gate_review_head.rs::the_hook_refuses_a_stale_head_under_human_instead_of_asking`)
are renamed by the build to the verb.

**lines.md, deny rules section:** the line on `gate.key` stays (section 8).

**roles/secretary/rules.md:54**, the last clause of "The lines you hold": "you never merge: you ask
makibad with merge, which lands only what the merge gate allows, and under merge_approval_mode
human only once the person approves it with their key."

**skills/pull-request/SKILL.md**, "Merging" (from the paragraph after the stop lines to the end of
the section), new text:

> To merge, call mcp__makiba__merge with `repo`, `pr`, `head` (the full 40-character head of the
> CI notice) and `method` `squash`. You never run `gh pr merge` or any other merge or enqueue
> command: the merge gate hook refuses them all. makibad runs the acceptance gate (the checks
> below); on a refusal it answers the verdict and nothing runs. Never try another way after a
> refusal: tell the person.
>
> - `accepted`: makibad merges (or, on a branch that requires the merge queue, enqueues) the pull
>   request at that head itself. End your turn; its notice says merged, enqueued, refused by GitHub
>   or not known. Nothing is retried: on a refusal or an unknown outcome, deal with the cause and
>   call merge again.
> - `pending` (under `human`, or when the gate warns): tell the person the pull request, its link
>   and the warnings, and that they approve it in their shell with `makiba merge approve <id>`. A
>   word in the conversation does not merge it. End your turn; a notice follows the approval.
>
> When the pull request leaves the merge queue without landing, the PR watch tells you; read why,
> fix it or call merge again (the gate judges it again, and under `human` the person approves
> again).

In "Merging", the `human` bullet becomes: "**`human`** (also when you do not know): once the newest
notice for the head reads it mergeable now, call merge; it answers `pending`, and only the person's
key lets it land." The paragraph "Your merge gate hook runs the acceptance gate..." keeps its list of
checks under the heading "What the gate checks", and its last sentences become: "The hook refuses
every way to merge or enqueue from a command line. Write every GitHub CLI, curl or wget command
plainly ... (unchanged)." The description line and `when_to_use` say "the person approves a merge"
instead of "the person says to merge".

**docs/design/spec.md**: section 3 (the gate key, retired), section 12 (the `merge` verb's row and
`makiba merge`; `gate` for the person only), section 5.6 (the PR watch's classification and the
queue), and the read surface tables if `status` lists pending merges. **docs/runbooks/self-hosting.md**
step 4 (the gate's question through the hook) is replaced by a step that approves a pending entry.

**docs/design/merge-wait.md**: section 2's `human` bullet (lines 46-48) says more than today's code;
under this design it holds (the person's key). Its Q3 is answered by D3. Build Issues 3 and 4 are
replaced (section 12 of this note).

**roles/secretary/settings.json**: an allow rule for `mcp__makiba__merge`, so the secretary's
permission layer does not ask about the call.

## 10. Claims about GitHub and gh

Verified, from this repository's code (behaviour makibad already relies on): `gh pr view --json
state,mergedAt,mergedBy,headRefOid,mergeCommit,...` gives the fields the PR watch reads
(`prwatch.rs:814-815`); makibad's gh and the secretary's find the same stored login
(`gh.rs:17-22`, `launch.rs:296-310`).

[unverified], to be read before or during the build (Issue 1 confirms 1-4 and records the source in
this section):

1. `PUT /repos/{o}/{r}/pulls/{n}/merge` with `sha` refuses when the head is not `sha`, and answers
   `merged` and the merge commit's `sha`; its error statuses. Source:
   https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request
2. On a branch that requires the merge queue, the REST merge is refused, and the GraphQL mutation
   `enqueuePullRequest` with `expectedHeadOid` enqueues bound to that head; the queue's merge method
   is the branch's setting. Source: https://docs.github.com/en/graphql/reference/mutations#enqueuepullrequest
3. `PullRequest` has `isMergeQueueEnabled`, `isInMergeQueue` and `mergeQueueEntry`, readable in one
   query; whether `gh pr view --json` exposes any of them. Source:
   https://docs.github.com/en/graphql/reference/objects#pullrequest
4. gh writes an API error's message to stderr and exits non-zero (`gh api`). Source:
   https://cli.github.com/manual/gh_api
5. Whether a pull request leaving the queue to land can be read `OPEN` with no entry for a moment.
6. What `mergedBy` reads after a queue merge; whether a removal from the queue carries a readable
   reason (a timeline event).
7. `gh pr merge` on a queue branch, and its `--match-head-commit` there (merge-wait.md claim 5):
   no longer needed by this design, which does not use `gh pr merge`.

## 11. Proposed build Issues (not filed until approved)

1. **The `merge` verb and its records.** What: `Verb::Merge` (spec.rs), the gate split in
   `door.rs` (section 3.1), `merge_attempted` in the verb's transaction, the pass
   `src/daemon/merge.rs` with `boot` and `running` (section 4.3), the REST merge and the enqueue,
   the outcome events and notices, the restart reconciliation; first confirm claims 1-4 of section
   10. Depends on: 5. Acceptance: a fake gh shows an allowed request merged at the judged head with
   `merge_done`, a refused one sending nothing, and an open attempt of another `boot` reconciled as
   `merge_unknown` `observed` `merged` and never sent again.
2. **The person's approval of a pending merge.** What: the `pending` kind `merge` (a schema step),
   `request` under `human` writing the entry, `approve` / `refuse` / `withdraw`, `makiba merge
   approve <id>` with its preview, `answer` refusing the kind, withdrawal on a new head or a
   landing. Depends on: 1. Acceptance: under `human` the secretary's request lands nothing until the
   person's key approves the printed entry; a changed entry or a new warning answers `stale`.
3. **The hook with no allowed form.** What: `merge_gate.rs` refuses every `gh pr merge` and the
   `enqueuePullRequest` mutation and asks no door; the gate key retired (section 8). Depends on: 1
   (the secretary needs the verb first). Acceptance: the former allowed form and an enqueue mutation
   are refused naming the verb; the hook's tests pass with makibad down.
4. **PR watch classification from the act record, and queue ejection.** What: `apply_state`'s
   `merged_via` from act records (`gate`, `unknown`, `hand`), the wait while an attempt is open, the
   queue entry in the state read and `merge_dequeued` with its notice. Depends on: 1, and on Issue
   424 ask 1 (the notice of every merge), which lands first. Acceptance: an `acceptance_gate` allow
   alone gives `hand`; a `merge_unknown` gives `unknown`; a fake queue read that drops the entry
   twice writes one `merge_dequeued` and one notice.
5. **gh.rs keeps stderr.** What: `run_bounded` collects stderr (bounded) and `GhError::Failed`
   carries it. Depends on: nothing. Acceptance: a fake gh that writes an error and exits 1 gives
   that text in the error; existing callers' tests pass.
6. **The docs and lines rows.** What: the texts of section 9 (lines.md L-K2, L-K9, L-K10;
   rules.md:54; SKILL.md "Merging"; spec.md sections 3, 5.6 and 12; the self-hosting runbook step;
   merge-wait.md build Issues 3 and 4 marked replaced), the secretary's allow rule. Depends on: 1-4
   (the rows name their tests), and on PR 402 for L-K9. Acceptance: `tests/role_settings.rs` finds
   every reference the rows name; `python3 tools/check_language.py` passes.

Files the build changes: `src/spec.rs`, `src/daemon/door.rs`, `src/daemon/mod.rs`,
`src/daemon/merge.rs` (new), `src/daemon/prwatch.rs`, `src/daemon/gh.rs`, `src/daemon/gate.rs`
(only if the gate's `pr_view` is shared with the act's read), `src/daemon/person_key.rs`,
`src/daemon/launch.rs`, `src/daemon/belt.rs` (a pending merge on a range task counts as waiting for
the person), `src/hooks/merge_gate.rs`, `src/cli.rs`, `src/status_local.rs`, `src/db/mod.rs` and a
new schema step, `roles/secretary/settings.json`, `roles/secretary/rules.md`,
`skills/pull-request/SKILL.md`, `docs/design/lines.md`, `docs/design/spec.md`,
`docs/design/merge-wait.md`, `docs/runbooks/self-hosting.md`, and tests (`tests/merge_act.rs` new;
the merge gate and gate review tests renamed or rewritten for the verb).

## 12. What this replaces

**merge-wait.md build Issue 3** ("confirm that the one allowed command enqueues with the expected
head; if not, change the allowed form"): replaced by build Issues 1 and 3 here. There is no allowed
command left; makibad enqueues with `expectedHeadOid` itself. The rest of that Issue stays with
#360: the review relaxation of Q5 (for (ii), the gate merges the head onto the current base and
applies `same_changes` before it allows), and L-K9's added clause on the queue's checks, which
build Issue 6 here writes together with this note's L-K9 text.

**merge-wait.md build Issue 4** ("PR watch: read the queue state ... tell the secretary when a pull
request leaves the queue without landing; keep `merged_via` keyed on `headRefOid`"): replaced by
build Issue 4 here, with one change: `merged_via` is keyed on makibad's act record at
`headRefOid`, not on the gate's allowance.

**Issue 424**: its ask 2 (call a merge `gate` only when the command is known to have run; an
allowance counting only for a merge seen within some minutes) is replaced by section 7: there is no
window, the act record is the evidence. Its ask 1 (tell the secretary of every merge, whatever made
it) is not replaced; it is built by 424, first, and build Issue 4 here depends on it.
