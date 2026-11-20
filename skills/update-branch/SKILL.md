---
name: update-branch
description: Take the base branch into a pushed worker branch with a merge commit, so the next push needs no force, then record the new head.
when_to_use: Main moved under a branch that is already pushed (a pull request is open or CI is watched), and the branch must take main in.
---

# update-branch

Once a branch is pushed you never force-push it, so it takes the base in as a merge commit and the next push is a fast-forward. A branch that is not pushed yet is the worker's to rebase: tell it with mcp__makiba__send.

**Precondition:** your sandbox must let you write in the worker's worktree and the project repository's git folder (Issue 183). Until it does, tell the person the commands to run.

## Steps, in the worker's worktree

1. `git fetch origin`.
2. `git status --porcelain --untracked-files=no` must print nothing. If tracked files have uncommitted changes, stop: they are the worker's; ask it with mcp__makiba__send.
3. `git merge-base --is-ancestor origin/<base> HEAD` succeeds: the branch already contains the base. Stop.
4. `git merge --no-edit origin/<base>`. On a conflict, `git merge --abort`, which leaves the branch as it was, and send the conflict to the worker to resolve; it reports done again.
5. `git push origin <branch>` (no force), then mcp__makiba__pr with `state` `pushed` and the new full `head`, as in the pull-request skill.

Never rebase a pushed branch, never `--force`, never reset. Your hooks refuse the dangerous forms.

A merged branch whose pull request has a test-first acceptance declaration still keeps that test commit first: the merge does not rewrite it.
