---
name: delegate
description: Hand one task to a new worker with the dispatch tool. The dispatcher makes the worktree or the working folder, writes the worker's settings and launches it; the secretary writes the brief and registers a project with the project tool.
when_to_use: A request is ready to hand to a worker; a worker sends "base is stale"; a launch failed and the notice came to you.
---

# delegate

Start one worker for one task with mcp__makiba__dispatch. You never make a worktree, write a settings file or start a terminal yourself: `makibad` does all three, and a launch it could not verify comes back to you as a notice.

## Preconditions

- The project is registered: its repository is in the repositories folder (`$MAKIBA_REPOS_DIR/<project>`, spec section 2). A request for a repository that is not there: call mcp__makiba__project with `slug` (the project's name, as `dispatch`'s `project`) and `repo` (`OWNER/NAME` on GitHub, never a URL or a path). It answers `accepted`; `makibad` clones it and a notice says when it is done or why it failed. Dispatch only after the notice says done (a code dispatch of the project is refused while it runs).
- A `project` refused `denied`: the owner is not in the organization's `allowed_repo_owners`. Ask the person to register it themselves (`makiba project <slug> <OWNER/NAME>`) or to allow the owner (`makiba config set allowed_repo_owners <owners>`); never guess another owner. A bare name with no owner: ask the person for `OWNER/NAME`.
- A request that names no repository at all: ask the person which repository it is for. makiba does not make a local repository yet. A document request needs no repository (see the document skill).
- The worker branches from the project repository's default branch as it is on the machine. When the base must be newer, call mcp__makiba__project again for the project: on an existing clone it fetches and fast-forwards the default branch.

## Dispatch

Call mcp__makiba__dispatch with:

- `kind`: `code` (a worktree of the project) or `document` (a working folder; use the document skill).
- `task_id`: short, lowercase letters, digits, `_` and `-`, at most 30 characters, unique among the tasks in progress. The worker's id is `w-<task_id>`.
- `title`: a few words.
- `project`: the project's slug (required for `code`).
- `brief`: the task text (below).
- `model` (optional): only when the person asks for a model for this task.
- `branch` (optional): the branch name, when the person or the repository wants one.
- `issue` (optional): the Issue number the task works on. Give it whenever the task is one Issue: a range of the project then does not dispatch that Issue again (`makibad` reads it from the run). A task without it is invisible to a range.

A refusal names its kind: `usage` (fix the arguments), `conflict` (the task id is in use: pick another), `unavailable` (`makibad` is not running: tell the person once and wait).

## The brief

Give the task one worker with an exact scope. Write it in English:

1. What to change, and why, in a few sentences.
2. What not to touch.
3. How to check it: the repository's own tests and lint, and any test the task adds.
4. How to report: `report` `done` with what changed, the commit, how it was checked and what was not done; `blocked` with one question when it cannot go on.
5. The commit form the repository uses. Workers never push: you do (the pull-request skill).
6. When the task is an Issue: the Issue's body, and the comments that bear on it, word for word (`gh issue view <N> --repo <owner>/<name> --json title,body,comments`), not your summary. The worker cannot read GitHub, and a summary drops the points it would have checked.
7. When the branch is to declare an acceptance test (its first commit, which cannot change afterwards): write the test and the change uncommitted and see the test fail without the change and pass with it, with lint and format, before the first commit; keep the declared test to the Issue's definition of done; and report, before that commit, any reading of the Issue wider or narrower than its text and any premise it does not state.

Never put a token, a password or a path of the person's own machine that the task does not need in a brief.

## After the dispatch

- The worker's `start`, `done` and `blocked` reports reach you as messages. End your turn and wait for them.
- A launch that failed (the folder was not there, the first turn never came, a startup dialog not on the policy list) comes to you as a notice from `makibad`. Read the reason, tell the person when it needs them, and dispatch again with a new task id once the cause is fixed.
- To see what a worker's terminal shows, call mcp__makiba__peek with the task id. Do not infer a worker's state from screen text alone.
- mcp__makiba__status shows the dispatcher's heartbeat. Until Issue 168 it shows nothing about runs: what happened to a launch comes to you as a message.

## A worker says "base is stale"

A worker reads its base from the project repository's `origin/<branch>`. Call mcp__makiba__project with the project's slug and repository; when the notice says it is done, answer the worker with mcp__makiba__send: "origin/<branch> is updated to <sha>: run git fetch origin" (the sha is in the notice). Its `origin` is the project's repository (Issue 241), so the fetch brings that sha; it rebases itself. If the notice says it failed, tell the person the reason.
