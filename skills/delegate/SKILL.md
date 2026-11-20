---
name: delegate
description: Hand one task to a new worker with the dispatch tool. The dispatcher makes the worktree or the working folder, writes the worker's settings and launches it; the secretary writes the brief.
when_to_use: A request is ready to hand to a worker; a worker sends "base is stale"; a launch failed and the notice came to you.
---

# delegate

Start one worker for one task with mcp__makiba__dispatch. You never make a worktree, write a settings file or start a terminal yourself: `makibad` does all three, and a launch it could not verify comes back to you as a notice.

## Preconditions

- The project is registered: its repository is in the repositories folder (`$MAKIBA_REPOS_DIR/<project>`, spec section 2), on the branch work starts from. **Limit:** no tool registers or clones a project. A request that names a repository that is not there, or names none, goes to the person: ask them to register it, and say that you cannot (a verb for it is Issue 184). A document request needs no repository (see the document skill).
- The worker branches from the project repository's own default branch as it is on the machine; `makibad` does not fetch. When the base must be newer, the project repository has to be updated first (see "base is stale" below).

## Dispatch

Call mcp__makiba__dispatch with:

- `kind`: `code` (a worktree of the project) or `document` (a working folder; use the document skill).
- `task_id`: short, lowercase letters, digits, `_` and `-`, at most 30 characters, unique among the tasks in progress. The worker's id is `w-<task_id>`.
- `title`: a few words.
- `project`: the project's slug (required for `code`).
- `brief`: the task text (below).
- `model` (optional): only when the person asks for a model for this task.
- `branch` (optional): the branch name, when the person or the repository wants one.

A refusal names its kind: `usage` (fix the arguments), `conflict` (the task id is in use: pick another), `unavailable` (`makibad` is not running: tell the person once and wait).

## The brief

Give the task one worker with an exact scope. Write it in English:

1. What to change, and why, in a few sentences.
2. What not to touch.
3. How to check it: the repository's own tests and lint, and any test the task adds.
4. How to report: `report` `done` with what changed, the commit, how it was checked and what was not done; `blocked` with one question when it cannot go on.
5. The commit form the repository uses. Workers never push: you do (the pull-request skill).

Never put a token, a password or a path of the person's own machine that the task does not need in a brief.

## After the dispatch

- The worker's `start`, `done` and `blocked` reports reach you as messages. End your turn and wait for them.
- A launch that failed (the folder was not there, the first turn never came, a startup dialog not on the policy list) comes to you as a notice from `makibad`. Read the reason, tell the person when it needs them, and dispatch again with a new task id once the cause is fixed.
- To see what a worker's terminal shows, call mcp__makiba__peek with the task id. Do not infer a worker's state from screen text alone.
- mcp__makiba__status shows the dispatcher's heartbeat. Until Issue 168 it shows nothing about runs: what happened to a launch comes to you as a message.

## A worker says "base is stale"

A worker reads its base from the project repository's `origin/<branch>`. Update the project repository (`git fetch origin` in it), then answer the worker with mcp__makiba__send: "origin/<branch> is updated to <sha>". It rebases itself.

**Precondition:** your sandbox does not let you write in the project repositories, and no tool fetches the project repository alone yet (mcp__makiba__update fetches only for a run's pushed branch). Until one does, tell the person the command to run.
