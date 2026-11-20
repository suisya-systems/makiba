---
name: retro
description: Look back on how a delegation went once its worker has finished, record what is reusable about delegating as a raw note, and make sure every failure met ends in a check, an issue, or a stated reason why it cannot be checked.
when_to_use: A worker's task is finished (merged, handed over, or abandoned) and is about to be closed.
---

# retro

A look back on the delegation itself: how you split, briefed and followed the task. What the worker learned about the work (an API's quirk, a trap in the code) is the worker's own note, saved with the curate skill; it is not repeated here. Run it before you close the worker, so its note is counted with that close.

## 1. Look back

- **Task size:** was the task too large or too small for one worker?
- **The brief:** could the worker work without asking? How many questions came back, and could the brief have answered them?
- **The project:** was it the right repository and base?
- **Parallelism:** were there too many or too few workers at once?
- **The report:** was the done report enough to explain the result to the person?

## 2. Decide whether to record

Record:

- a pattern you are likely to meet again in the same kind of delegation;
- something that would improve how you write briefs;
- a constraint of the project that will matter next time;
- a worker note that was too thin or too long, and what would have been right.

Do not record a one-off problem of this task, or what the worker's note already says.

## 3. Every failure ends in a check

For each failure you looked at in step 1, do one of these. The look back is not finished while a failure has none of them. A note is not a check.

1. **Add a check:** a hook, a test or a tool check that fails when the failure happens again. Have it made by a worker (the delegate skill), and make sure the brief asks the worker to show that the check fails when the failure is brought back.
2. **Open an issue for the check** (the issue skill): what the check is and how to show it fails. Keep the issue number in the note.
3. **When it cannot be a check:** one line saying why.

## 4. Record

When there is something to record, save a raw note with mcp__makiba__note as the curate skill describes, with these differences:

- `topic`: `delegation-<topic>`, so the file is `raw/<YYYY-MM-DD>-delegation-<topic>.md`. The `delegation-` prefix tells it apart from the workers' notes.
- `task`: the task_id of the task you looked back on (`makibad` writes it as the first line `Task: <task_id>`).
- The four headings: `## Facts`, `## Judgment`, `## Grounds`, `## Where it applies`. Add the check, issue number or reason of step 3 under `## Judgment`.

Whether a pattern becomes a skill is decided by the curator's gates and the person (the curate skill), not here.

## 5. Report

When you recorded something, tell the person in one line: "I recorded a lesson on delegation: <topic>." Otherwise say nothing.
