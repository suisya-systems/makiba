# A pull request that fails acceptance by construction, landed on the owner's one answer (Issue 574)

Decide note for Issue 574. Goal clause G5: what must not happen is stopped by something outside the
agent. Design only: nothing is built until the owner picks an option and answers the questions of
section 7; section 8 lists the build Issues. Code references were read at 2ff3983 and checked again at 9666228 (origin/main; #561 and #564 moved lines of `door.rs` only).

## 1. What is true today

### 1.1 Where a failing `acceptance` stops a landing

- **GitHub.** `main` requires `tests`, `lint`, `real-base` and `acceptance` (classic branch
  protection, `strict` false), and a ruleset requires the merge queue (merge-wait.md section 5,
  read 2026-11-24). A pull request whose `acceptance` check failed at its head is not merged or
  enqueued, by makibad or by hand; the owner's lift (remove the requirement, merge in the web UI,
  restore it) is what landed #530, #519, #520, #535 and #534. That GitHub refuses the enqueue itself
  for a failed required check at the pull request's head, rather than only failing the queue's
  build, is [unverified]; the five lifts show only that the owner could not land them otherwise.
- **The job.** `acceptance` (`.github/workflows/ci.yml`, job `acceptance`) runs the **base's**
  `scripts/acceptance-check.sh` on `pull_request` (`git show "$base:scripts/acceptance-check.sh"`),
  so a pull request cannot change the rules it is judged by. On `merge_group` it judges nothing
  and succeeds (Issue 401, merge-wait.md Q2): the declared test is judged at the pull request's
  head. At a test-commit head it fails at once (test-commit-scoped-ci.md).
- **The checker's fixed lines.** Every failure is one `no` line (`acceptance-check: <text>`,
  `scripts/acceptance-check.sh`, function `no`), then `ACCEPTANCE: fail`. The kinds of the Issue
  map to these lines:
  - (a) #530, a declared test edited after the test commit: `<path> changed after the test commit
    (declared tests, ...)` (rule 4);
  - (b) #520, #535, a declared Cargo target ignores a test: `<path> ignores a test at head <sha>:
    test result: ...` (rule 3). The `test result` text holds a duration (`finished in 0.52s`), so
    the line is not the same from one run to the next;
  - (c) #534, the checker changed after its test commit: `scripts/acceptance-check.sh changed after
    the test commit (...)` (rule 4), the same line as (a) with another path;
  - (d) #519, a stand-in under `/tmp` only in the job's build: `<path> fails at head <sha>:` and the
    last 20 lines of the test output. Issue 558 removes the cause.
  The checker prints its own lines at column 0 and the tests' output only indented (`sed 's/^/  /'`)
  or not at all; but the pull request's code runs in the same job and could write to the job's
  output another way, so **a line in the log is the job's statement only for display, never for
  binding** (section 4.3).
- **makiba's gate** (`src/daemon/gate.rs`). Before the gate runs, the door requires that the newest
  `ci_completed` of the pull request is `passed` at that head (`src/daemon/door.rs` `gate_outside`,
  about line 3487); a failed `acceptance` makes it `failed`, so today a merge request on such a head
  never reaches the gate. Condition (b) passes when the newest completed run of the check named in
  `.github/acceptance-check` (`acceptance`) at head succeeded: `Ci::check_runs` (about line 936)
  reads `check-runs?filter=all` and keeps, per (app, name, workflow), the run with the latest
  `completed_at`, so a rerun's success supersedes the failed first attempt (`tests/acceptance_gate.rs`
  about line 705). Else the fallback passes: a CI job failed at the test commit and succeeded at
  head, on one base; the fallback can pass while `acceptance` failed, which today GitHub's
  requirement still stops. Condition (a) protects the declared paths, test
  infrastructure and every path the test commit changed (`check_protected`); it does **not** protect
  `scripts/acceptance-check.sh` (`is_protected` is `is_infra` or under a declared path). So kind (a)
  is refused by the gate twice (condition (a), and (b) since `acceptance` failed), kinds (b) and (c)
  only by (b). The gate does not read whether a test was ignored.
- **The person's answer on a merge.** A pending entry of kind `merge` is approved with `makiba merge
  approve <id>` (the person's key, over the printed text's SHA-256 `shown`; merge-act.md section 5).
  `makiba answer <id> yes` on a merge entry is refused and names `merge approve`
  (`src/daemon/door.rs`, the `answered` path, about line 1150). **The Issue's text says `makiba
  answer <id> yes`; the code's one answer for a merge is `makiba merge approve <id>`.** This note
  uses the latter; it is still one answer.
- **Credentials.** makibad runs `gh` with the person's stored login, the same login the secretary is
  given (merge-act.md section 6). So anything makibad writes to GitHub with that login (a commit
  status, a comment, a rerun) **the secretary can write too**. A check run can be created only by a
  GitHub App [unverified: the Checks API's write is limited to Apps]; makiba has none. Every agent's
  sandbox, the secretary's included, refuses reading the person key, the retired gate key and the
  ledger (`src/daemon/prepare.rs` `deny_files`, about line 447, outside the role test); `~/.ssh` and
  the stored credential files are refused to workers and the curator only, **not to the
  secretary** (`credentials()`, about line 87, skipped for `Role::Secretary` about line 420). The read
  surface masks the person and gate keys (`src/status_local.rs` about line 958), and so do the
  reader's masks (`src/daemon/review.rs` about lines 826, 886, 1393).

### 1.2 What follows

A gate answer alone cannot land these pull requests: what `acceptance` reports at the head, or what
is required, must change. And whatever makes the change must be something no agent can produce:
not the person's GitHub login (the secretary holds it), not a file an agent can read.

## 2. Options

### Option 1: the acceptance job accepts a signed approval for exactly that head and failure set

The base's checker, when its only failures are of an allowed kind (section 5), looks for an approval
naming the repository, the pull request, the head and the exact set of those failures, signed by a
key whose public half the **base** holds under `.github/`. With one that verifies it passes, and says
so in its last line. The required checks stay as they are.

- Forgery: needs the private key; posting the approval text is open to anyone (it is data, verified,
  not trusted).
- Stale: another head, another pull request or another failure set does not verify; the job fails
  and the gate asks again.
- Merge group: nothing new (section 6).
- One-time acts by the owner: commit the public key under `.github/` and the workflow step that
  hands the approvals to the checker (`.github/` is the owner's by hand); land the checker change once
  by the lift (it is kind (c) itself).
- makibad keeps running only git and `gh`; it holds no signing key (section 3).

### Option 2: a makibad-owned required check replaces `acceptance`

makibad reports a check that is success when `acceptance` passed or the owner approved this head's
failure; `acceptance` becomes informative.

- Forgery: a commit status written with the person's login is the secretary's to write too, so the
  required check must be bound to a source the agents do not hold: a GitHub App of makiba's, its
  private key out of every agent's reach, and branch protection naming that App as the check's
  expected source [unverified: classic protection's `checks[].app_id` binding for a status]. A new
  App, its installation and its key are new infrastructure.
- makibad becomes the source of a required check for **every** pull request and every queue commit
  (it must report on `gh-readonly-queue/main/pr-<n>-<sha>` too): while makibad is down nothing lands,
  not even by the owner's hand, and an `acceptance` pass is no longer what GitHub enforces but what
  makibad relays.
- One-time act by the owner: change the required checks; create and install the App.

### Option 3: keep the lift, prepared by makibad

One pending entry per batch names every pull request, kind, head and order; after the owner's yes
makibad tells the exact settings to change and verifies them before and after. It shortens the lift
but keeps it: `main` still lacks the required check during the window, and the owner still changes
branch protection by hand, which is what the Issue asks to remove.

## 3. Recommendation: option 1, signed by a key of its own

Option 1 keeps GitHub's bound (the required check, run by GitHub Actions from the base's checker)
and adds one input to it that only the owner's command can make. Option 2 moves the bound from
Actions to makibad and needs an App; option 3 does not remove the hand change.

Which key signs. Not the owner's SSH key: `~/.ssh` is readable by the secretary (1.1), so a
signature by it is not something no agent can make. A key of its own, kept as the person key is:

- **`approval.key`**: an ed25519 SSH private key beside the person key (`$MAKIBA_APPROVAL_KEY`,
  default `approval.key` next to `person_key_path()`, as `gate_key_path()` is resolved), mode 0600
  in the 0700 folder, created by `makiba up` when absent, with its public line printed for the owner
  (`makiba up` and a read-only `makiba approval-key`). The **resolved** path, default or custom, is
  added to `deny_files` for every role (the secretary included, as the person key is, outside the
  role test), to the reader's masks in `review.rs` and to the read surface's mask in
  `status_local.rs`.
- **Who signs**: `makiba merge approve` (the person's command, in the owner's terminal) signs with
  `ssh-keygen -Y sign -n makiba-acceptance-approval -f <approval.key>`; only the signature goes
  through the door. Bound: no agent can read the key, and the person's command is the only signer;
  an agent that escapes its sandbox reaches the person key as well, so this is the same bound as the
  person key's, no weaker.

The public half is committed by the owner as an allowed-signers file `.github/acceptance-approvers`
(`ssh-keygen -Y verify` format: one line `makiba-owner namespaces="makiba-acceptance-approval"
<public key>`), read by the checker **from the base** (`git show "$base:.github/acceptance-approvers"`),
so a pull request cannot add its own signer. No file at base: no approval is honoured. Rotating the
key is a new `approval.key` and the owner's edit of that file.

## 4. The approval

### 4.1 The canonical failure lines (checker)

The checker prints, before its last line, each failure once more in a canonical form that holds no
duration, no output and no sha:

- `acceptance-check: approvable changed <path>`: rule 4's line for a declared path or a path the
  test commit changed (kind a), or for `scripts/acceptance-check.sh` (kind c);
- `acceptance-check: approvable ignored <path> <n>`: rule 3's "ignores a test" with `n` ignored
  tests (kind b);
- `acceptance-check: not approvable`: once, when any other failure was found (section 5), and
  when a failure's path holds a byte outside printable ASCII without space (`0x21` to `0x7e`): the
  checker supports names with newlines, spaces and other bytes (`tests/test_acceptance_helper_change.py`
  about line 311), and such a name could split or impersonate a record, so it keeps the lift.

So every path in a record is one plain token, and a record is `changed <path>` or `ignored <path>
<n>`, with no other encoding to share between the checker, makibad and the command. The **failure
set** is the sorted list of the records (bytewise). Its SHA-256 (records joined by `\n`, with a final
`\n`) is the **failure digest**.

### 4.2 The approval text

Exactly these lines, ASCII, each ending in `\n`:

```
makiba-acceptance-approval v1
repo <owner/name>
pr <number>
head <40 hex>
failures <failure digest>
pending <pending id>
<one line per failure, as in the failure set>
```

Signed with namespace `makiba-acceptance-approval`. It is carried in a pull request comment whose
body is the approval text, a line `---`, and the armored `SSH SIGNATURE` block. A commit status's
description (140 characters) cannot hold an SSH signature; a comment can, is visible to every
reader of the pull request, and costs no new permission beyond reading comments.

### 4.3 How the job decides

`ci.yml`'s acceptance step (owner's change) reads every comment of the pull request that starts
with `makiba-acceptance-approval v1` (with the job's `GITHUB_TOKEN`, `pull-requests: read`), writes
each to a file under `$RUNNER_TEMP/approvals/`, and passes the folder to the base's checker
(`--approvals <dir>`). The checker, after its own judgement and only when it would fail:

1. fails as today when any failure is not approvable, or when the failure set is empty;
2. reads `.github/acceptance-approvers` from the base; none: fails as today;
3. for each file: splits the text and the signature, verifies with `ssh-keygen -Y verify -f
   <signers> -I makiba-owner -n makiba-acceptance-approval -s <sig>` reading the text, and accepts it
   only when `repo` is `$GITHUB_REPOSITORY`, `pr` is the event's pull request number, `head` is the
   head it judged, and the listed failures equal **its own** failure set (recomputed in this run,
   never read from a log) with the matching digest;
4. one accepted file: prints `acceptance-check: approved by the owner (pending <id>): <set>` and
   `ACCEPTANCE: pass`; otherwise `ACCEPTANCE: fail` as today.

So a forged comment (no valid signature), a stale one (another head or pull request), one that
leaves out a failure or names one that is not there, and lines a test printed into the log, all
fail closed.

### 4.4 makibad's part

1. **The ask.** A merge request whose newest `ci_completed` at that head is `failed` takes a path of
   its own in `gate_outside`, before today's "no passed ci_completed" refusal: it goes on only when
   the newest completed run of every check at head (as `Ci::check_runs` reads them) succeeded except
   the repository's acceptance check, and that one failed. It reads that job's log (`repos/<repo>/
   actions/jobs/<id>/logs` through the API; that the check run's id is the job's id is [unverified])
   and takes the checker's lines. A downloaded job log starts every line with the runner's
   timestamp and one space (`2026-11-27T10:00:00.1234567Z acceptance-check: ...`): makibad strips
   exactly one leading token of that form (an RFC 3339 UTC time ending in `Z`) and its one space,
   keeps the rest of the line as it is, and reads only lines that then start with `acceptance-check:
   ` at column 0, so an indented line of test output stays indented and is not read. From them it
   takes the `approvable` records and `not approvable`. Any `not
   approvable`, no record, or a log it cannot read: refused, as today. Then the gate is evaluated as
   for a passed head, every rule unchanged (the manifest, condition (a), `check_first`, the review,
   edited tests, the helper lists, the language and order checks), except two: condition (b) is not
   judged (no route of it, the fallback included, can stand in for the failed acceptance), and
   condition (a)'s refusals for exactly the paths of `changed` records are held back. Allowed: the
   answer carries the warning `acceptance fails by construction: <set>` beside the gate's other
   warnings, so it is the existing pending merge entry (Issue 432) under both modes; its text names
   the pull request, the head, the method, each record, every other warning, and the approval text.
2. **The answer.** `makiba merge approve <id>` (the person) checks `shown` as today, signs the
   approval text, and sends the signature with `id` and `shown`. The door runs the ask again (1) and,
   in its final transaction, by the same conditional update on the entry (warnings equal to the
   entry's, or fewer), writes `acceptance_approved` (`repo`, `pr`, `head`, `method`, the failure
   digest, the set, the entry's `warnings` other than the acceptance warning, `pending_id`, the
   comment body) instead of `merge_attempted`, and tells the secretary: post this comment, rerun the
   failed `acceptance` job, record the rerun (the `pr` verb's `rerun`, an existing route). makibad
   writes nothing to GitHub, and checks the signature only for its form; the job checks it.
3. **The landing, on the same answer.** When the rerun's `ci_completed` passes at that head, the
   secretary asks `merge request` again with the same method. With an `acceptance_approved` of that
   repository, pull request, head and method, the door evaluates the gate as for any passed head,
   with condition (a)'s refusals for the approval's `changed` paths held back. When the gate allows
   with no warning outside the approval's recorded warnings (the rule `merge approve` applies: those,
   or fewer), the request writes `merge_attempted` with `approved_by` `person` and the approval's
   `pending_id`, under both modes, with no new entry: the person's one answer was over that method,
   head and those warnings. A warning not among them makes a new pending entry, as today. Another
   head has no approval: the ask starts again.

State: no memory beside the ledger. `acceptance_approved` is the record and the authorization; a
restart loses nothing (the comment is the secretary's to post, again if needed: a second identical
comment is harmless). One approval authorizes at most one `merge_attempted` (the request checks that
none names its `pending_id`, in the same transaction).

Why the secretary posts and reruns: the signature, not the poster, is the bound, so posting is no
privilege; the secretary already reruns CI (`ci_rerun`), and makibad gains no GitHub write.

## 5. Allowed kinds

Allowed, each only on the owner's approval of that head:

- **changed**: a declared path, or a path the test commit changed, changed after the test commit
  (kind a, #530), whether it is a test target (a declared `tests/<name>.rs` or `test_*.py`), a
  declared test file that is not a target, or a helper the test commit changed. Precedence: a path
  under `.github/` or that is test infrastructure (`is_infra`) is never approvable, whatever else it
  is; otherwise a declared path or a path the test commit changed is kind (a);
- **changed** `scripts/acceptance-check.sh` after the test commit (kind c, #534);
- **ignored**: a declared Cargo target that ignores a test at head (kind b, #520, #535).

Not allowed, so still by the lift:

- any change under `.github/` (the workflow is what runs the verification, and the gate's condition
  (b) requires `.github/workflows/` equal to the base);
- test infrastructure under `tests/` (`is_infra`) changed after the test commit, declared or not;
- a file under `tests/` that is neither a test target nor a helper, and that is neither declared nor
  changed by the test commit, changed after the test commit;
- a declared test that fails at head (kind d, #519, until Issue 558), passes or runs no test at the
  test commit, does not compile at head, runs no test at head;
- rule 1 (the test commit changes an undeclared path), a second declaring commit, a merge test
  commit, a merge bringing in a non-base commit, a worktree or listing failure.

A kind not listed needs the lift, as today. Widening the list is a change of the base's checker,
so it is itself a kind (c) pull request.

## 6. The merge queue's merge group

Its commit (`gh-readonly-queue/main/pr-<n>-<base>`) differs from the pull request's head. Nothing
changes for it: on `merge_group` `acceptance` judges nothing and succeeds (Issue 401), the gate
judged the pull request's head before the enqueue, and `tests`, `lint`, `real-base` run on the
queue's commit as for any entry. The approval binds the pull request's head only and is not read on
the queue's commit. Known limit: if the `merge_group` step ever judges again, the approval would have
to be found through the pull request number in the queue branch's name.

## 7. Questions for the owner

- Q1. Option 1 (recommended), 2 or 3?
- Q2. The signing key: `approval.key` beside the person key, denied to every agent (the secretary
  included) at its resolved path, signed by `makiba merge approve` (recommended)? The owner's own
  SSH key is not offered: the secretary can read `~/.ssh`.
- Q3. The allowed kinds of section 5: as listed (recommended), or narrower (for example, not the
  checker change)?
- Q4. The secretary posts the comment and reruns the job (recommended), or makibad does both with
  the person's login (a new GitHub write in makibad, a pass like `merge.rs`)?
- Q5. The one answer is `makiba merge approve <id>`, not `makiba answer <id> yes` as the Issue
  says: agreed?

## 8. Build Issues (option 1)

Order: 1, 2, 3, 4, then 5 (the owner's, last). The checker must know `--approvals` before the
workflow passes it: a base checker that does not know the flag answers usage (exit 2) and would fail
every pull request's `acceptance`. The key (3) must exist before its public line is committed (5).
Until 5 is in, the route is inert: an approval is asked, signed and
recorded, the rerun fails again, and the lift remains.

1. **The checker prints its failures in canonical form and honours a signed approval.**
   `scripts/acceptance-check.sh`: the `approvable` and `not approvable` lines of 4.1 (no verdict
   changes without `--approvals`), and `--approvals <dir>` as in 4.3; `docs/acceptance-check.md`
   (the route, the allowed kinds, the limits). Declared test `tests/test_acceptance_approval.py`
   (temporary repositories and an `ssh-keygen` key made in the test's temporary folder, as
   `tests/test_acceptance_check.py`): one case per allowed kind (its line, with no duration in it)
   and per non-allowed kind (`not approvable`), each verdict unchanged without `--approvals`; a valid
   approval passes; an edited declared Rust target, an edited declared `test_*.py` and an edited
   helper the test commit changed are each `approvable changed`, while an edited `tests/conftest.py`
   (declared or not) and an edited `.github/` file are `not approvable`; a signature by another key, another head, another pull request, a missing or an
   extra failure, no signers file at base, a signers file only at head, and a `not approvable`
   failure each fail. It changes the checker after its test commit: it lands by the lift, the last
   one this route needs (one pull request, so one lift, not two).
2. **The gate judges a head whose acceptance failed by construction.** `src/daemon/gate.rs`: the
   job's log read (a stand-in serves it), the warning, condition (a)'s refusals held back only for
   the set's paths, every other failure refused as today, condition (b) not judged on that path.
   Declared test `tests/gate_acceptance_approval.rs`, its logs in the downloaded form (each line after
   a runner timestamp): a log with only approvable records gives an allowance with the acceptance
   warning naming the set; a timestamped indented line is not read; `not approvable`, no record or an unreadable
   log is refused; a fabricated indented line is not read; a declared path outside the set is still
   refused by (a); a head whose acceptance failed is not allowed by the fallback of (b). Changes
   `docs/design/spec.md` (the `acceptance_gate` payload).
3. **The approval key.** `src/cli.rs` (`approval_key_path`, `$MAKIBA_APPROVAL_KEY`), `makiba up`
   (creates it with `ssh-keygen`, prints the public line), a read-only `makiba approval-key`;
   `src/daemon/prepare.rs` `deny_files` for every role, `src/daemon/review.rs` masks,
   `src/status_local.rs` mask. Declared test `tests/approval_key.rs`: the key is made 0600 in a 0700
   folder; the secretary's, a worker's and the curator's settings deny its resolved path, at the
   default and at a custom `MAKIBA_APPROVAL_KEY`; the reader's boundary and the read surface do not
   show it. Changes the reader's read boundary (Issue 262's): it shows less, never more.
4. **`merge approve` signs, records `acceptance_approved`, and the next request lands on it.**
   `src/cli.rs` (`merge approve` signs), `src/daemon/door.rs` (the ask's path in `gate_outside`,
   the event in the approval's transaction, the notice with the comment body, the next request's
   authorization), `src/daemon/gate.rs` (the evaluation without (b), (a) held back). New event kind:
   `src/spec.rs`, `tests/spec_matches_code.rs`, `docs/design/spec.md` event table. Declared test
   `tests/acceptance_approved.rs` (a key made by `ssh-keygen` in the test's temporary folder, a
   stand-in for the CLI), under `acceptance` and `human` each: a failed acceptance at a head where
   every other check passed gives one pending entry; another failed check is refused; the event and
   the notice after `approve`; the next request at the same head and method writes `merge_attempted`
   with `approved_by` `person` and no new entry; a second request does not write a second attempt; a
   new warning makes a new entry; another head or method has no approval; a stale `shown` writes
   nothing.
5. **The owner's `.github/` change (by hand, after 1 and 3 have landed).** `.github/acceptance-approvers`
   with `approval.key`'s public line; `ci.yml`'s acceptance step reads the approval comments and passes
   `--approvals`; job permission `pull-requests: read`. Test `tests/test_ci_acceptance_approval.py`
   (reads `ci.yml`: the step, its permission, the checker taken from the base).

Each code Issue is a decide-then-build Issue under the repository's rules (the merge gate, the
acceptance checker and `.github/`), so each stops after its design for the owner.

## 9. Known limits

- A pull request whose branch was cut before build Issue 5 runs the old workflow (the
  `pull_request` workflow comes from the pull request's merge commit): it needs a base merge first.
- That a rerun of a failed required check turns GitHub's requirement green, and that the rerun reuses
  the event's base and head, is [unverified]; the secretary's reruns of CI (`ci_rerun`) are the
  nearest evidence. Establish it on the first use, and keep the lift as the fallback.
- An approval cannot be withdrawn once posted, but it is bound to one head and one failure set; a
  push makes it void.
- Kind (d) stays with the lift until Issue 558 lands. Issues 558 and 576 change the same checker:
  their builds and build Issue 1 above must be ordered so no two change `scripts/acceptance-check.sh` at once.
- The owner's terminal must reach `approval.key` when they approve; a person on another machine
  cannot approve this way.

## 10. Design review

Round 1, reader codex, verdict passed, findings blocker 2, major 2, minor 1. Each was checked
against the code and taken in:

- Blocker (held): the secretary can read `~/.ssh` (`prepare.rs` `credentials()` is skipped for the
  secretary), and a custom key path was unprotected. The owner's SSH key is dropped; `approval.key`
  is denied at its resolved path to every role and masked, tested at default and custom paths
  (section 3, build Issue 3).
- Blocker (held): `gate_outside` refuses before the gate unless the newest `ci_completed` passed at
  head, and the gate checks condition (a) before (b) and its fallback can pass with a failed
  acceptance. The ask is its own path before that refusal, requires every other check to pass, and
  judges every rule but (b) (section 4.4, 1; build Issue 2 tests the fallback).
- Major (held): under `human` a secretary's request always makes a new entry (`door.rs` about line
  4874), so a second request would ask the person again. `acceptance_approved` records the method and
  the warnings shown and authorizes one `merge_attempted` for that head, under both modes (4.4, 3).
- Major (held): newline-delimited raw paths are ambiguous, and newline names are supported. A path
  outside printable ASCII without space is not approvable, so each record is plain tokens (4.1).
- Minor (held): `Ci::check_runs` already keeps the newest completed run per key, and a test covers a
  later success over a failure. The baseline (1.1) is corrected and the "not established" point
  dropped.

## 11. The owner's decision

Pending entry 119, answered yes: option 1 (Q1), with `approval.key` beside the person key, denied to
every agent, signing when the person runs `makiba merge approve <id>` (Q2, Q5); the secretary posts
the approval as a pull request comment and reruns `acceptance` (Q4); the allowed kinds of section 5,
(a), (b) and (c), everything else keeping the lift (Q3); and the five build Issues of section 8,
which the secretary files.
