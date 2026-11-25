---
name: update-branch
description: Have makibad take the base branch into a pushed worker branch with a merge commit and push it, so no push is ever forced, then record the new head.
when_to_use: A branch that is already pushed must take main in, and makibad does not do it on its own: makibad updates a pull request it follows itself when its queue readiness rule says so (docs/design/queue-ready.md), and a move of main is otherwise no reason to update.
---

# update-branch

Once a branch is pushed it is never force-pushed, so it takes the base in as a merge commit and the push is a fast-forward. `makibad` does it for you with mcp__makiba__update; you never run git in a worker's worktree yourself. A branch that is not pushed yet is the worker's to rebase: tell it with mcp__makiba__send.

## Steps

1. mcp__makiba__update with `task` (the task id) and `base` (the pull request's base branch, for example `main`). It answers `accepted`; end your turn.
2. `makibad` fetches the base and the branch from origin into the project's repository, merges `origin/<base>` into the worktree's head there when the branch does not hold it yet (a merge commit, never a rebase), pushes the result, never forced, and sends you a notice. The worker's worktree is not changed (Issue 241).
3. The notice names the head at origin: call mcp__makiba__pr with `state` `pushed`, the pull request's `repo`, `pr` and `task`, and that full `head`, as in the pull-request skill. When the notice says nothing was merged and the head is the one already recorded, there is nothing to record.
4. When something was merged, tell the worker with mcp__makiba__send: "origin/<branch> is updated to <head>: run git fetch origin, then git merge --ff-only origin/<branch>, and report done in one line ("fast-forwarded to <head>, nothing committed")". If it has committed since, the fast-forward fails: it then runs git merge origin/<branch> (a merge commit, never a rebase) and reports done again, and you push with mcp__makiba__push.
5. The notice says it failed:
   - A conflict: nothing was merged and the notice names the files. Send them to the worker with mcp__makiba__send to resolve (it merges the base itself and commits); it reports done again, and you push with mcp__makiba__push.
   - Uncommitted changes, or the worktree on another branch: they are the worker's; ask it with mcp__makiba__send, then call update again.
   - The branch is not at origin: it was never pushed; it is the worker's to rebase.
   - The branch moved at origin: somebody else pushed to it. Tell the person; nothing was merged or pushed.
   - The push failed after the merge (git's message, for example a missing credential): nothing changed at origin or in the worktree; once the cause is dealt with, call update again.

Never rebase a pushed branch, never force, never reset. Your hooks refuse the dangerous forms.

A merged branch whose pull request has a test-first acceptance declaration still keeps that test commit first: the merge does not rewrite it.
