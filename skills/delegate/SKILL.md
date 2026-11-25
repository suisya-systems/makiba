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

- `design_first` (optional): `true` when the person must read the task's design before its test is fixed: a task on a trust boundary (git, the sandbox, keys, the push line, the merge gate or the acceptance checker), or one the person asked to see the design of. `makibad` then refuses the task's test commit (a commit with an `Acceptance-Test:` trailer), at the push and in the worker's commit hook, until the person answers `yes` to a pending entry of the task with `stop_kind` `design` (Issue 337, line L-K13). When the worker reports its design blocked, add that question with mcp__makiba__pending (`add` with the five parts, `stop_kind` `design`, `recommend_answer`; then `escalate`), and ask the person to answer it in their own shell with `makiba answer <id> yes` (their key). Do not carry their yes with mcp__makiba__answer: an answer through your tool is recorded as yours, does not open the test commit, and closes the entry. If that happened, add the design question again (a new entry), escalate it and ask the person to answer that one. A design you approve yourself, within one module, is not dispatched with `design_first`.

A refusal names its kind: `usage` (fix the arguments), `conflict` (the task id is in use: pick another), `unavailable` (`makibad` is not running: tell the person once and wait).

## The brief

Give the task one worker with an exact scope. Write it in English:

1. What to change, and why, in a few sentences.
2. What not to touch. A change to an existing test, a file under `.github/`, a script, the merge gate, the acceptance checker, or the language check (`tools/language-allowlist.txt`, or `tools/check_language.py`, its scope) needs the person: the brief says the worker stops and asks before making one (Issue 339).
3. How to check it: the repository's own tests and lint, and any test the task adds.
4. How to report: `report` `done` with what changed, the commit, how it was checked and what was not done; `blocked` with one question when it cannot go on.
5. The commit form the repository uses. Workers never push: you do (the pull-request skill).
6. When the task is an Issue: give `issue`, and do not paste the Issue. `makibad` reads its title, body and comments at dispatch and puts them after your brief, as a snapshot; the worker cannot read GitHub. Add only what the Issue does not say (which comment overrides the body, what changed since it was written). When the answer's `issue_text` is `unreadable: ...`, send the worker the Issue's text word for word.
7. When the branch is to declare an acceptance test (its first commit, which cannot change afterwards): write the test and the change uncommitted and see the test fail without the change and pass with it, with lint and format, before the first commit; keep the declared test to the Issue's definition of done; and report, before that commit, any reading of the Issue wider or narrower than its text and any premise it does not state.
8. Name only the instruction files the repository has: look in its clone for `CLAUDE.md` and `AGENTS.md` before you tell the worker to read one. A brief that names a missing file stops the worker at once.
9. When the brief carries a cause, a reading or a summary that comes from outside makiba (another tool, another organization, a pasted analysis): put it in only marked as a hypothesis, with where it came from, never as a finding. Ask the worker to establish the cause from the primary record (the full log, the code) and to say in its report whether the hypothesis held.

Never put a token, a password or a path of the person's own machine that the task does not need in a brief.

## After the dispatch

- The worker's `start`, `done` and `blocked` reports reach you as messages. End your turn and wait for them.
- A launch that failed (the folder was not there, the first turn never came, a startup dialog not on the policy list) comes to you as a notice from `makibad`. Read the reason, tell the person when it needs them, and dispatch again with a new task id once the cause is fixed.
- To see what a worker's terminal shows, call mcp__makiba__peek with the task id. Do not infer a worker's state from screen text alone.
- mcp__makiba__status shows the dispatcher's heartbeat. Until Issue 168 it shows nothing about runs: what happened to a launch comes to you as a message.

## A worker says "base is stale"

A worker reads its base from the project repository's `origin/<branch>`. Call mcp__makiba__project with the project's slug and repository; when the notice says it is done, answer the worker with mcp__makiba__send: "origin/<branch> is updated to <sha>: run git fetch origin" (the sha is in the notice). Its `origin` is the project's repository (Issue 241), so the fetch brings that sha; it rebases itself. If the notice says it failed, tell the person the reason.
