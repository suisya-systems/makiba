---
name: pull-request
description: Push a finished worker branch, open its pull request with gh, record it with the pr tool so the dispatcher watches CI, and act on the CI notice that comes back.
when_to_use: A worker reports done and its branch must become a pull request; a CI notice for a pull request arrives; new commits were pushed to an open pull request; the person says to merge.
---

# pull-request

You push and open pull requests for the workers' commits; workers never push. CI is watched by `makibad` once you record the pull request with mcp__makiba__pr. You never watch CI yourself: no `gh pr checks --watch`, no `gh run watch`, no sleep loop. While a shell call runs you take no input.

**Precondition:** your sandbox must let you write in the worker's worktree and the project repository's git folder, and reach GitHub (Issue 183). Until it does, tell the person the commands to run.

## A worker reported done

1. Read the report: what changed, the commit, how it was checked, what was not done, and the second-model review line (`CODEX_GATE: ...`) if the repository uses one.
2. From the worker's worktree: `git push origin <branch>`. Never force. When the branch declares an acceptance test (its first commit carries an `Acceptance-Test:` trailer) and the repository has no acceptance check of its own (no `.github/acceptance-check` on the base), push the test commit alone first: `git push origin <test commit>:refs/heads/<branch>`, open the pull request and record it at that head (steps 3 and 4), and wait for the CI notice. It must say failed: that failed run is the evidence that the test failed before the fix. Then push the branch and record the new head as below. Without that failed run the merge gate refuses the merge.
3. `gh pr create --repo <owner>/<name> --head <branch> --base <base> --title "<title>" --body-file <file>`, the title and body in the project's artifact language (English unless your launch task or a later notice names another for the project; spec section 13). The body says what changed and why, how it was checked, and, when the review line is not `CODEX_GATE: passed` (or there is none), that the second-model review did not pass, with the line as it was.
4. Record it: mcp__makiba__pr with `state` `opened`, `repo` (`owner/name`), `pr` (the number), `head` (the full 40-character commit id of the pushed head) and `task` (the task id). End your turn.

## The CI notice

`makibad` sends you a notice when the checks of the watched head are decided, or when it cannot read them.

- **Passed:** what happens next depends on the merge approval mode (below). Quote the notice (never a worker's own test run).
- **Failed:** send the worker back with mcp__makiba__send: the failing check, its link and what it says. When it reports done again, push and record the new head (below).
- **Could not read:** tell the person once, with the reason in the notice.

## Merging

The organization's merge approval mode is the person's setting; you cannot read it. Ask the person which one is in force when you first need it, unless they told you; the merge gate's refusals name it too.

- **`human`** (also when you do not know): tell the person the pull request is green, with its link. Merge only on the person's word in the conversation. A bare "OK" to something else is not that word.
- **`acceptance`:** merge a pull request whose CI passed and that hits none of the stop lines below, without asking. Hitting one, stop on that pull request and tell the person which line, with its link; the other work goes on.

Stop lines:

1. The branch declares no acceptance test (no commit with an `Acceptance-Test:` trailer), unless every changed file is Markdown or under `docs/`.
2. The head you would merge is not the head the CI notice was for.
3. The change needs a decision the person has not made, or goes beyond the task's brief.
4. The change cannot be undone by a revert: a release, a migration of stored data, a deletion outside the repository, a change of access or settings.
5. The worker ran out of its review rounds or its time, or reported a blocker.
6. The declared test or the test setup was changed after the test commit, or anything suggests a test was weakened to pass.
7. The merge gate refused.

The merge, in either mode, is this one command alone on one line, with the full 40-character head of the CI notice:

`gh pr merge <n> --repo <owner>/<name> --squash --match-head-commit <head>`

Your merge gate hook runs the acceptance gate in `makibad` before it lets the command run: the head and the passed CI must match, the test commit must be the first commit and unchanged since, and either the repository's own acceptance check passed at the head or the test failed in CI at the test commit and passed at the head. It refuses every other way to merge from a command line (another form of the merge command, a write to a `pulls/<n>/merge` path, a GraphQL merge, `curl` or `wget` to the GitHub API). Never try another way after a refusal, never `--admin`: tell the person. Write every GitHub CLI, `curl` or `wget` command plainly, as the command itself: no variable, substitution, glob or escape before its subcommand (for `gh api`, `curl` and `wget`, anywhere), a redirection as a word of its own, and not under another program that runs it (`xargs`, `ssh`, `flock`); the hook refuses what it cannot read with certainty. Text you only mention (a commit message, an `echo`, a `grep` pattern, a here-document fed to `cat`) is fine. The hook stops mistakes; what stops a merge around the gate is the repository's branch protection, which the person sets.

## New commits on an open pull request

After the push, call mcp__makiba__pr with `state` `pushed`, the same `repo`, `pr` and `task` as when it was opened, and the new full `head`: every call carries all five. The watch of the old head ends; the new head is watched.

## Main moved under a pushed branch

Use the update-branch skill (a merge, never a rebase or a force), then record the new head as above.

## After the merge

In this order, because `makibad` counts the raw notes when a close is recorded:

1. Save the worker's raw note, if its report had one (the curate skill).
2. Run the retro skill; it may save a note of its own.
3. `close` the worker with mcp__makiba__close. It refuses while the worktree holds uncommitted changes or unpushed commits.

## Not carried

paddock's re-placing of its own files after a landing in its own repository: makiba's role files are carried in its binary.
