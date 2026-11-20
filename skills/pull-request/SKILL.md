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
2. From the worker's worktree: `git push origin <branch>`. Never force.
3. `gh pr create --repo <owner>/<name> --head <branch> --base <base> --title "<title>" --body-file <file>`, the title and body in the project's artifact language (English unless your launch task or a later notice names another for the project; spec section 13). The body says what changed and why, how it was checked, and, when the review line is not `CODEX_GATE: passed` (or there is none), that the second-model review did not pass, with the line as it was.
4. Record it: mcp__makiba__pr with `state` `opened`, `repo` (`owner/name`), `pr` (the number), `head` (the full 40-character commit id of the pushed head) and `task` (the task id). End your turn.

## The CI notice

`makibad` sends you a notice when the checks of the watched head are decided, or when it cannot read them.

- **Passed:** tell the person the pull request is green, with its link, quoting the notice (never a worker's own test run). Merge only on the person's word in the conversation: `gh pr merge <n> --repo <owner>/<name> --squash --match-head-commit <head>`. Never `--admin`, never a merge around a refusal. The merge approval mode and its gate are Issue 47; until then the person decides every merge.
- **Failed:** send the worker back with mcp__makiba__send: the failing check, its link and what it says. When it reports done again, push and record the new head (below).
- **Could not read:** tell the person once, with the reason in the notice.

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
