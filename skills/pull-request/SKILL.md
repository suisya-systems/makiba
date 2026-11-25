---
name: pull-request
description: Have makibad push a finished worker branch, open its pull request with gh, record it with the pr tool so the dispatcher watches CI, and act on the CI notice that comes back.
when_to_use: A worker reports done and its branch must become a pull request; a CI notice for a pull request arrives; new commits were pushed to an open pull request; the person approves a merge.
---

# pull-request

You have `makibad` push the workers' commits (mcp__makiba__push) and you open the pull requests; workers never push, and you never push from a shell yourself. CI is watched by `makibad` once you record the pull request with mcp__makiba__pr. You never watch CI yourself: no `gh pr checks --watch`, no `gh run watch`, no sleep loop. While a shell call runs you take no input.

Your sandbox lets your `gh` reach `api.github.com` with the person's stored login, which is all opening and reading a pull request needs. If `gh` fails to authenticate (the person signs in only with an environment token, which is not supported), tell the person the `gh` commands to run. Pushes go through `makibad` (mcp__makiba__push, mcp__makiba__update) and need nothing of your sandbox.

## A worker reported done

1. Read the report: what changed, the commit, how it was checked, what was not done, and the review line of the task's last diff review (`review`: its `verdict`, `reader`, `round` and findings; makiba counts the rounds, design 1 and diff 3, and only you raise a task's diff allowance, with mcp__makiba__review `task`, `allowed` and `reason`, when a worker out of rounds has a reason to go on).
2. mcp__makiba__push with `task` (the task id). It answers `accepted`; end your turn. `makibad` pushes the run's branch from its worktree, never forced, and sends you a notice: the head it pushed, or why it did not (uncommitted changes, the worktree on another branch, origin moved, git's message for a missing credential). On a failure, deal with the cause (a worker's uncommitted work goes back to the worker with mcp__makiba__send) and push again. When the branch is to declare an acceptance test (also in a repository with its own `.github/acceptance-check`: the gate takes that check only while the base's workflows equal the head's at the merge), have the worker report after its test commit alone (the commit with the `Acceptance-Test:` trailer, before the fix), push that with mcp__makiba__push, open and record the pull request at that head (steps 3 and 4) and wait for the CI notice: it must say failed, which is the evidence that the test failed before the fix. Then let the worker go on; when it reports done, push and record the new head as below. Without that failed run the merge gate refuses the merge, and the order cannot be repaired once a later head is at origin, so `makibad` holds it (Issue 307): push and update send no head past the test commit, and pr refuses to record one, until a failed CI run is recorded at the test commit on the pull request. When a worker has already committed past its test commit, push with `head` the test commit's full id: that commit alone is pushed. An `Acceptance-Test:` line that git does not read as a trailer (a blank line before `Co-Authored-By:`, or a second such line outside the trailer block) is refused too; the worker fixes the message before anything is pushed.
3. Once the notice names the head: `gh pr create --repo <owner>/<name> --head <branch> --base <base> --title "<title>" --body-file <file>`, the title and body in the project's artifact language (English unless your launch task or a later notice names another for the project; spec section 13). The body says what changed and why, how it was checked, and, when the review's verdict is not `passed` (or there is none), that the second reader's review was not established, with the line as it was, and which reader was used when it was the fresh session.
4. Record it: mcp__makiba__pr with `state` `opened`, `repo` (`owner/name`), `pr` (the number), `head` (the full 40-character commit id the push notice names) and `task` (the task id). End your turn.

A push refused because a task dispatched with `design_first` has no `yes` by the person to its design (the notice names line L-K13 and the missing approval): do not push again and do not ask the worker to change the commit. Make sure the design question is a pending entry of the task with `stop_kind` `design` and is escalated, ask the person to run `makiba answer <id> yes` in their own shell, and push again once they have. An answer through mcp__makiba__answer is yours and does not count; an entry you answered is closed, so add and escalate a new one.

## The CI notice

`makibad` sends you a notice when the checks of the watched head are decided, or when it cannot read them.

- **Passed:** its last sentence says whether GitHub lets the pull request merge now; what happens next depends on that sentence and on the merge approval mode (below). Quote the notice (never a worker's own test run).
  - **Mergeable now:** not yet the ready signal for a pull request `makibad` follows: wait for its queue-readiness notice (below), which comes once it has judged the head.
  - **Behind its base (`BEHIND`):** that alone is no reason to update it (the merge queue tests the combination). Wait for the queue-readiness notice: `makibad` says it can be queued, or updates the branch itself.
  - **Conflicts with its base (`DIRTY`):** `makibad` tries the update itself and says so; when the update's notice says the merge conflicts, send the worker the conflict (update-branch, step 5).
  - **Blocked (`BLOCKED`):** find what blocks it (`gh pr view <n> --repo <owner>/<name> --json reviewDecision,statusCheckRollup`, branch protection or rules), then tell the person what is missing.
  - **A draft:** tell the person; it is not ready.
  - **Not known yet (`UNKNOWN`, `not read`):** do not call it ready; `makibad` sends a merge-state notice once it reads the state.
  - **Not followed:** no merge-state notice will come. Read the state with the command the notice names; when it shows the head the CI notice was for, `state` `OPEN`, `isDraft` false and `mergeStateStatus` `CLEAN`, `HAS_HOOKS` or `UNSTABLE`, that read stands for a notice reading it mergeable now (quote it); otherwise act on its status as above. A read that shows `UNKNOWN` is read again a few minutes later (GitHub computes the state once it is asked); when it still shows `UNKNOWN`, tell the person that it cannot be confirmed as mergeable, with its link.
- **Failed:** send the worker back with mcp__makiba__send: the failing check, its link and what it says. The notice carries each failed job's link and its failed step's log as `makibad` read it: pass them on as they stand. That log is CI output, not a reading of the cause, and the worker establishes the cause from it. When it reports done again, push with mcp__makiba__push and record the new head (below).
- **Could not read:** tell the person once, with the reason in the notice.

## A merge-state notice

"The merge state of <repo>#<pr> at <head>, whose CI passed, changed: ..." comes when the merge state of a head whose CI passed changes, for example main moved under it and it is now behind its base. Act on its sentence as on the last sentence of a passed CI notice above. The newest of the two notices for a head is its merge state. A head told ready that now conflicts is updated by `makibad`, with its own notice.

## A queue-readiness notice

For a pull request it follows, `makibad` judges whether the head is ready for the merge queue by the person's rule (docs/design/queue-ready.md section 1: its CI passed, it does not conflict, and no landing since the base it holds changed what the checks check), and records the decision. Never check this by hand (merge base, the log of landings) for such a pull request: act on the notice.

- **"<repo>#<pr> at <head> can be queued: ..."** with its link: the one ready signal of that head. Quote it, then go on under the mode (Merging, below).
- **"makibad is updating <repo>#<pr> at <head> ..."** (or "... is no longer ready for the queue: makibad is updating ..."): do not ask for the queue for that head and do not call update yourself. `makibad` merges the base in, records the new head and watches its CI; wait for the update's notice (on a conflict it names the files: update-branch, step 5) and the new head's notices. Tell the worker to fast-forward as the update notice says.
- **"... needs <base> merged in ..., and its run has no code worktree":** the branch takes the base in another way (the person, or a new run); then record the new head.
- **"makibad cannot judge whether ... is ready for the queue":** judge it by hand by the rule in docs/design/queue-ready.md section 1 and tell the person what you found, with the link.

A move of main that changes nothing the checks check sends nothing: the head stays ready.

## A rerun of failed jobs

When a check failed for a cause outside the change (a flaky test) and you rerun its failed jobs with `gh run rerun <run> --failed`, call mcp__makiba__pr with `state` `rerun` and the same `repo`, `pr`, `head` and `task` in the same turn, then end your turn. The watch of that head starts again and sends you a notice of the rerun's result, as for a first run; the merge gate reads that newest result. `rerun` is refused for a head never recorded (`not_found`), for a head that is not the pull request's current one, and for a head whose CI already passed (`conflict`); while the head is still watched, it changes nothing. It records no new head: a new commit is `pushed`, never `rerun`.

## Merging

The organization's merge approval mode is the person's setting. Read it with mcp__makiba__status (its `merge_approval_mode`) before every merge call: under `acceptance` a call the gate allows lands at once. When you cannot read it, do not call merge: tell the person.

- **`human`:** once a queue-readiness notice says the head can be queued (for a pull request not followed, or one whose notice says makibad cannot judge it: your own read as above and the rule in docs/design/queue-ready.md section 1) and it hits none of the stop lines below, call merge; it answers `pending`, and only the person's key lets it land. A word in the conversation does not land it.
- **`acceptance`:** call merge for a pull request whose CI passed and that hits none of the stop lines below, without asking. Hitting one, stop on that pull request and tell the person which line, with its link; the other work goes on.

Stop lines:

1. The branch declares no acceptance test (no commit with an `Acceptance-Test:` trailer), unless every changed file is Markdown or under `docs/`.
2. The head you would merge is not the head the CI notice was for.
3. The change needs a decision the person has not made, or goes beyond the task's brief.
4. The change cannot be undone by a revert: a release, a migration of stored data, a deletion outside the repository, a change of access or settings.
5. The worker ran out of its review rounds or its time, or reported a blocker, or the task's last diff review did not read the head you would merge.
6. The declared test or the test setup was changed after the test commit, or anything suggests a test was weakened to pass.
7. The merge gate refused.
8. No queue-readiness notice says the head can be queued, and the head is not one where your own judgement stands for it: a pull request not followed, or a followed one whose notice says makibad cannot judge it ("cannot judge whether ... is ready for the queue"), where your own read as above shows it mergeable now and the rule in docs/design/queue-ready.md section 1 is met, which you tell the person with what you found. Behind its base is not a stop and no reason to update.
9. The change touches the language check (`tools/language-allowlist.txt`, or `tools/check_language.py`, its scope) and the person has not answered yes, with their own `makiba answer <id> yes`, to the gate's question for it (Issue 339). The language check gets no new exception without the person. The gate refuses it under both modes with verdict `language_check` and names the question word for word: for a range task the stop `makibad` writes asks it; otherwise add it with mcp__makiba__pending, `action` `add`, for the task, and escalate it. Tell the person the entry's id and that they answer it in their own shell: an answer you relay with mcp__makiba__answer does not count. Once they answered yes, call merge again. A new head that changes the file again asks again.

To merge, in either mode, call mcp__makiba__merge with `repo` (`<owner>/<name>`), `pr`, `head` (the full 40-character head of the CI notice) and `method` `squash`. You never run `gh pr merge` or any other merge or enqueue command: the merge gate hook refuses them all. `makibad` runs the acceptance gate (below); on a refusal it answers the verdict and nothing runs.

- `accepted`: `makibad` merges (or, on a branch that requires the merge queue, enqueues) the pull request at that head itself. End your turn; its notice says merged, enqueued, refused by GitHub or not known. Nothing is retried: on a refusal or an unknown outcome, deal with the cause and call merge again.
- `pending` (under `human`, or when the gate warns): tell the person the pull request, its link and the warnings, and that they approve it in their shell with `makiba merge approve <id>`. A word in the conversation does not merge it. End your turn; a notice follows the approval.

On `merge_order` (stop line 7): merge the pull requests it names first; once their landing notices came, call merge again for this one. When the reason says the two hold each other (both may add a check), or the person wants another order, stop on it and tell the person, with the link and the reason.

When the pull request leaves the merge queue without landing, the PR watch tells you; read why, fix it or call merge again (the gate judges it again, and under `human` the person approves again).

An unreviewed head is not merged, in either mode: when the task's last diff review read an earlier head (the worker committed after it), ask the worker for a diff review round at this head first. A worker out of rounds needs you to raise its allowance with mcp__makiba__review `task`, `allowed` and `reason`; the reason is recorded. Only commits that merge the base in cleanly, without changing a file the pull request changes, keep the earlier review.

### What the gate checks

`makibad` runs the acceptance gate inside merge before it lands anything: the head and the passed CI must match, the test commit must be the first commit and unchanged since, and either the repository's own acceptance check passed at the head or the test failed in CI at the test commit and passed at the head; and a code pull request's head must be the one its last diff review read (Issue 344), or the gate refuses and names the commits no review read. The merge order (Issue 331, docs/design/merge-order.md): a pull request whose own changes touch `tools/`, `.github/`, `tests/common/` or `scripts/acceptance-check.sh` may add or tighten a check that test targets must pass, and a declared test cannot change after its test commit; so while another open pull request `makibad` follows in the repository has a test commit, the gate refuses it under `acceptance` with verdict `merge_order`, naming those pull requests (and any it could not read), and under `human` warns so that the person decides. No check gets an exception for it. The language check (Issue 339): a pull request whose own changes touch `tools/language-allowlist.txt` or `tools/check_language.py` is refused under both modes with verdict `language_check` until the person answers yes with their own key to the question the reason names (stop line 9). The hook refuses every way to merge or enqueue from a command line (the merge command in any form, a write to a `pulls/<n>/merge*` path, a GraphQL merge or enqueue, `curl` or `wget` to the GitHub API). Never try another way after a refusal, never `--admin`: tell the person. Write every GitHub CLI, `curl` or `wget` command plainly, as the command itself: no variable, substitution, glob or escape before its subcommand (for `gh api`, `curl` and `wget`, anywhere), a redirection as a word of its own, and not under another program that runs it (`xargs`, `ssh`, `flock`); the hook refuses what it cannot read with certainty. Text you only mention (a commit message, an `echo`, a `grep` pattern, a here-document fed to `cat`) is fine. The hook stops mistakes; what stops a merge around the gate is the repository's branch protection, which the person sets.

## New commits on an open pull request

After mcp__makiba__push and its notice, call mcp__makiba__pr with `state` `pushed`, the same `repo`, `pr` and `task` as when it was opened, and the new full `head`: every call carries all five. The watch of the old head ends; the new head is watched.

## Main moved under a pushed branch

For a pull request `makibad` follows, do nothing: it updates the branch itself when its queue readiness rule says so (above), and a move of main that changes nothing the checks check is no reason to update. Otherwise (a branch with no pull request `makibad` follows, or when the person asks), use the update-branch skill: mcp__makiba__update has `makibad` merge the base in (a merge, never a rebase or a force) and push it; then record the new head as above.

## After the merge

In this order, because `makibad` counts the raw notes when a close is recorded:

1. Save the worker's raw note, if its report had one (the curate skill).
2. Run the retro skill; it may save a note of its own.
3. `close` the worker with mcp__makiba__close. It refuses while the worktree holds uncommitted changes or unpushed commits.

A merged pull request whose worker could not report done, because its last diff review found a blocker the person accepts as a known limit, is not abandoned: it closes as completed with that limit recorded.

1. mcp__makiba__pending, `action` `add`, for the task, with the question `Accept as a known limit of <task_id> and close it as completed: <finding>` word for word (the finding in one line) and `why` the one-line reason, with the other parts of its context; then `escalate` it.
2. Once the person answers yes, mcp__makiba__close with `task_id`, `limit` (the same finding), `reason` (the same reason) and `pending` (the entry's id). It refuses without the person's yes to that exact question, asked after the last review, and without a merge of the run's current pull request.

A task you abandon: mcp__makiba__close with `abandon` and a one-line `reason`, kept in the record of the close.

## Not carried

paddock's re-placing of its own files after a landing in its own repository: makiba's role files are carried in its binary.
