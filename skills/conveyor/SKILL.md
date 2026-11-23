---
name: conveyor
description: Draft a range of work with the person, have them approve it with their key, and carry each task makibad dispatches inside it through the pull request, CI and the merge, stopping on each pull request as the approval mode and the stop lines of the pull-request skill say.
when_to_use: The person wants a scope of work to run without asking per task ("run these issues", "take the small bugs of this project until the budget is spent"); a range task reports done, asks, stops or lands; a range ends.
---

# conveyor

A belt: the person approves a range once, and `makibad` puts its next task in whenever a slot frees, by itself. The range is a record in `makibad` (`docs/design/belt.md`), not a sentence of yours: what is inside it, how many at once, the budget, the count of slots and the dispatch are `makibad`'s. What stays with you is judgment: drafting the range with the person, answering workers, the pull request, the merge under the mode, the close, and the stops.

This skill orders the other skills; it does none of their work. Questions are `makiba:escalation`; push, pull request, CI and the merge are `makiba:pull-request`; a stale branch is `makiba:update-branch`; the close is `makiba:curate`, `makiba:retro` and mcp__makiba__close.

## Lines this skill never crosses

- Only the person's key approves or resumes a range; `makibad` refuses yours. Never ask the person for their key or to approve "on your behalf"; ask them to run `makiba range approve <id>` in their own shell.
- Nothing widens an approved range. More Issues, more budget, more at once: a new proposal with `replaces`, approved again by the person.
- The merge follows the approval mode, read before every merge (below). The range never stands in for the acceptance gate, and the gate never stands in for the range.
- Only you run this skill. A worker is one task in one range.

## 1. Drafting the range

In the conversation, as often as the person likes:

1. Agree with the person on the project, the Issues (a list, a condition, or both), how many at once, how many tasks in all, and the brief head every task starts from (conventions, verification depth, what to stop and ask on).
2. Record the draft: mcp__makiba__range with `action` `propose`, `project`, `issues` and/or `takes` (`label:<l>`, `no-label:<l>`, `milestone:<m>`, `clause:<=G<n>`, joined by AND), `at_once`, `budget_tasks`, optionally `budget_hours` (hours from approval after which nothing new goes in) and `budget_fixes` (fixes of a task's pull request before it asks, 3 unless said), `follow_ups` (`inside` unless the person wants each follow-up asked for: `ask`), `design_ahead` (true when an Issue waiting for another of the range may do its design meanwhile), `brief_head`, and `replaces` (the previous draft, or the active range a wider one replaces). It dispatches nothing.
3. Read what it admits now: mcp__makiba__status, its `ranges[]` entry: `candidates[]`, each admitted or left out with why. Show the person that list, not your own reading of the Issues.
4. Put in the condition only what can be admitted with nobody looking. An Issue that needs a look first goes in the list, or is taken out with `action` `skip` (with `issue` and `reason`) before it is dispatched. A proposal whose Issues touch a trust boundary (git, the sandbox, keys, the push line) says in its brief head "stop after the design and ask".
5. When the draft is right, ask the person to approve it with `makiba range approve <id>`, which shows them the same read and asks before it approves.

## 2. While the range runs

- `makibad` dispatches each task itself (`r<range>-<issue>`), with the brief head and the Issue's text. You do not dispatch a range's Issue; a one-off dispatch of another Issue names it with `issue` (`makiba:delegate`), so the range does not take it again.
- A worker's question you cannot answer goes to the person through `makiba:escalation`, with `stop_kind` and `recommend_answer` (`makibad` refuses the escalation without them). The escalation frees the task's slot and the next task goes in at once.
- A stop `makibad` wrote itself reaches you as a notice naming its pending entry: the merge gate refused a task's pull request, a pull request edits tests it did not declare, a task used up its fixes, or the range reached a budget with Issues left. The entry is escalated already and its slot is filled. Put its question to the person with the pull request's link, and carry their answer with mcp__makiba__answer; the answer comes back to you, and what follows is yours (send the worker back, or leave the landing to the person).
- A follow-up you file on the way (`makiba:issue`, kind followup, with its `Follow-up of #<n>` line) is in a range of `follow_ups` `inside` and goes in by itself, unless its `Needs host:` or `Stop line:` line keeps it out; then it is listed as asked for, and only a new range takes it.
- With `design_ahead`, a task waiting for another of the range goes in for its design only. Its `report blocked` is its design: nothing to answer. It is parked until what it waits for has landed and is woken with what landed; a dependency that will not land, or whose launch failed, reaches you as a stop for the person.
- `makibad` pauses the range itself after two failed launches in a row, or two refused pull requests in a row. Tell the person why (the notice says); only they resume it.
- **Done** is `makiba:pull-request`: push, open the pull request, record it with mcp__makiba__pr (the landing is proved from that record). The range is the person's approval to push and open pull requests for its tasks.
- A merged pull request is closed with `makiba:curate`, `makiba:retro` and mcp__makiba__close, in that order; the close makes `makibad` read the range again.
- The range ends by itself when its budget (tasks or hours) is spent and its tasks are closed, or when nothing is left inside; it ends `scan_error` when its project cannot be read. Tell the person.
- The person says pause or end: mcp__makiba__range with `action` `pause` or `end` and the `id`. Work already running goes on to its own end. Resuming is the person's.

## 3. The merge

Read the approval mode with mcp__makiba__status before every merge you would make without the person's word: its `merge_approval_mode` is the setting now, and the range's `approval_mode` the one it was approved under.

- The merge gate holds the pair: a range task's pull request lands without the person only when both are `acceptance`; for any other pair (`human` in either) your merge command stops at a question to the person. So read the pair to know what to say, not to decide: both `acceptance`, go on below; otherwise tell the person the pull request is green, with its link, and wait for their word. The rest of the belt goes on.
- Both `acceptance`: check the stop lines of `makiba:pull-request` (its "Merging" section, stop lines 1 to 7). Hitting none, merge with the one command that section names. Hitting one, stop on that pull request and tell the person which line, with its link; the rest of the belt goes on.
- A refusal of the merge gate is stop line 7, and `makibad` has written it as a stop already (section 2): never another way, never `--admin`.

This read replaces, while a range is in force, the pull-request skill's "ask the person which mode is in force".

## 4. Showing the belt

The person sees the range in mcp__makiba__status (`ranges[].items[]`: every Issue with its place, landed ones with their pull request; `places`; `stops[]`). When you tell them where the belt stands, read it from there; do not keep a list of your own.

## 5. Renewal

The range is in the records: a renewed secretary's brief names the ranges in force first. Read mcp__makiba__status before anything else and go on from its `ranges[]`; the handover needs only what is judgment (what you told the person, what they said they would decide).

## Not carried

- A scope written in the conversation or a file, and your own count of slots, of a task's fixes or of failures in a row: the range and `makibad`'s counts replace them.
- Dispatching the range's next candidate yourself: `makibad` does it when a slot frees.
- Answering a stop with the recommendation on the person's behalf: not in this version; each stop records the recommendation and the answer so that the person can decide later.
- The review round cap's relay: the door counts the rounds (`review`); a worker out of rounds reports it (stop line 5), and only you raise its allowance, with a reason.
- A journal of the belt: `makibad` records every dispatch, push, pull request and close itself.
- A notice when a pull request is merged: you merge it yourself; a merge by the person on GitHub you learn from the person.
- A classifier of what a change touches and its evidence for the pull request: the worker says how it ran its change, and the pull request says how it was checked.
- Ports for parallel runs: the worker's and the project's own concern.
