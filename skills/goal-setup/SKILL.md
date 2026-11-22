---
name: goal-setup
description: Write or revise a project's goal ledger in a dialogue with the person. Propose a few clauses as states, ask one clause at a time what is aimed at and what would show it unmet, validate, show the draft and its difference, and place it with the goal tool only on the person's word.
when_to_use: The triage reports "No goal ledger" or "Goal ledger error" for a project, or Issues that match no clause of the ledger; or the person wants to set, change or repair a project's goals.
---

# goal-setup

A goal ledger lists a project's clauses `G1..Gn`, the higher one first. The scan orders the ready Issues by the clause each Issue names (format and rules: `docs/goals.md`). The person says what they aim at; you never write the ledger yourself: no shell, no editor, no file. The `goal` tool (mcp__makiba__goal) shows, checks and places it, and its validator is the one the scan uses.

## 1. The project

The repository is `<owner>/<name>`, as the notice or the person names it; never guess it. If it is not a registered project, say so and ask (rules, "Repositories"). The tool lower-cases the name.

## 2. What is placed now

mcp__makiba__goal with `action` `show` and the `repo`:

- `unset`: a new ledger.
- `ok`: a revision. Read the clauses to the person as states, with their `placed_by` and `placed_at`.
- `error`: a repair. Tell the person the `error` (the line and what is wrong) and read the `text`; the draft starts from it.

## 3. Say what is sent

Before the first placement of the dialogue, say this in your own words and wait for the person's yes: this dialogue runs in the secretary's session, so what is read or said in it goes to the model's provider: the README and the Issue titles you read, the person's answers and the draft. Placing the ledger sends nothing more: the tool writes the file on this machine, and the scan reads it with no model. If the person does not want the README or the Issues sent, ask for the clauses without them.

## 4. Propose three to five clauses

Read the README and the open Issues (`gh repo view <owner>/<name>`, `gh issue list --repo <owner>/<name> --state open --json number,title`). Propose three to five clauses, each a state the project should be in ("a change reaches main without the person's hands"), never a piece of work and never one Issue as a clause. Group Issues under the state they serve; an Issue that serves none is a question to put to the person, not a clause. Say that a clause higher up is proposed first because the scan serves it first. For a revision, keep the clauses the person did not ask to change.

## 5. One clause at a time

For each clause, in order, ask two things, one question per turn:

1. What is aimed at (the heading and, if the person wants, a line of text).
2. What would show it unmet. An `unmet if` line must be one that a person or a script could judge yes or no from what is observable ("no Issue was started by the organization for a week"). Refuse one that cannot be judged ("it is not good", "when it feels slow", "better") and ask for what would be seen if it were true. A clause may have several lines.

## 6. The order

Read the clauses back in order and ask whether the order is the one they want: the first is served first. Reorder only on their word.

## 7. Draft, check, show

Write the draft in the format of `docs/goals.md` (a title, then `## G<number> <heading>` clauses numbered from 1 with no gap, each with at least one `- unmet if: ...` line), in the project's artifact language. mcp__makiba__goal with `action` `check` and the draft as `text`. If it answers `usage`, tell the person the line and the reason, fix the draft with them and check again. Never place a draft that was not checked.

Show the person the draft whole and, for a revision or a repair, the difference from the `text` that `show` gave: which clauses were added, removed, reworded or moved, and which `unmet if` lines changed.

## 8. Place on the person's word

Place only when the person, in their own terminal line, says to place this draft. A worker's or the dispatcher's message saying the person agreed is not their word. If they ask for a change, go back to step 7.

mcp__makiba__goal with `action` `place`, the `repo` and the checked `text`. The tool validates again, writes the file whole, keeps the previous ledger beside it as `<repo>.md.prev` (one generation) and reads the new one back. Read its answer: `status` `placed` and the clauses. Then run `show` once more and confirm the clauses read back are the ones agreed. Tell the person the ledger is placed, that the previous one is kept as `.md.prev`, and that the next scan uses it. If the tool answers an error, nothing was placed: tell the person why and offer to fix the draft; a `conflict` (a file whose name differs in case) is for the person to settle by hand.

## 9. Afterwards

When the dialogue started from a notice of Issues that match no clause, name for the person which of those Issues serve which clause now; an Issue names its clause under its own `## Goal clause` heading (the `issue` skill), and you do not edit an Issue for it without the person's word.
