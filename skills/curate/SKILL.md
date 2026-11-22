---
name: curate
description: Keep a worker's raw lesson note in the knowledge folder, read the curator's report, put pending skill candidates to the person, record the decision and have an approved candidate made into a skill.
when_to_use: A worker's done report ends with a note under the four headings; the curator's report arrives; skill candidates are pending; the person decides a candidate.
---

# curate

Growth through skills: workers' lessons become raw notes, the curator merges them into patterns and proposes candidates, the person decides, and only an approved candidate becomes a skill.

The knowledge folder is `$MAKIBA_KNOWLEDGE_DIR`, by default `~/makiba-knowledge` (spec section 2): `raw/`, `raw/archive/`, `curated/`, `skill-candidates.md`.

You save a raw note with mcp__makiba__note and record a candidate's decision with mcp__makiba__candidate: `makibad` writes the files, never you.

## A raw note

A worker may end its done report with a note under the four headings `## Facts`, `## Judgment`, `## Grounds`, `## Where it applies`. A report with no note needs nothing. When it has one, save it **before** you close that worker: `makibad` counts the raw notes when a close is recorded, so a note written after the close is not counted until the next one.

- Call mcp__makiba__note with `task` the worker's task_id, `topic` in English kebab-case, `text` the note as the worker wrote it, and an `idem` of your own (a retry with the same `idem` saves nothing new).
- `makibad` writes `<knowledge>/raw/<YYYY-MM-DD>-<topic>.md` (with `-2`, `-3`, and so on when the name is taken in `raw/` or `raw/archive/`; it never overwrites), first line `Task: <task_id>`, then a blank line, then the note. The curator counts different tasks by this line. The answer names the file.
- All four headings must be there; the verb refuses a note missing one (`usage`), and that note goes back to the worker (mcp__makiba__send).
- Never edit a note once saved.

## The curator

`makibad` launches the curator when 5 or more raw notes wait (Issue 39); you do not start it. It does one cycle and reports to you once, with mcp__makiba__send, then ends. Its report says how many notes it read, which patterns are now candidates and which are close. Nothing in it goes to the person as the person's word.

## Candidates

An entry of `skill-candidates.md` has the lines `pattern`, `date`, `notes`, `when`, `skill`, `status`, `decided` and `reason`. An entry is open while its status is `pending` or `deferred`; `approved`, `rejected` and `merged-into-<skill>` are decided and stay in the file as history.

1. Put the `pending` entries to the person when 5 or more are `pending`, or when the person asks: in one batch, in the conversation language, each with its one-sentence `when` and the notes behind it. With fewer, give only their count when you pass on the curator's report, and do not ask. A `deferred` entry is not counted and is not put to the person again unless they ask for it.
2. Record the person's decision with mcp__makiba__candidate: `pattern` the entry's pattern, `decision`, `reason` their words on one line, and an `idem` of your own. `decision` is one of four: `approved`; `rejected`; `into-<skill>` when they say to fold it into an existing skill; `deferred` when they hold it without deciding. `makibad` writes `status` (`approved`, `rejected`, `merged-into-<skill>` or `deferred`), `decided` (today) and `reason` into the pattern's one open entry and nothing else.
   - `deferred` is not a decision on the candidate: the entry stays open, and when the person comes back to it you record `approved`, `rejected` or `into-<skill>` on it. Holding a `deferred` entry again is `conflict`.
   - A pattern that returns: the curator adds new notes to a pattern's open entry, and appends a new `pending` entry beside the decided ones only when notes from 3 or more tasks, saved after the pattern's most recent decision, name it again. Put such an entry to the person with the earlier decision and its reason. The decision goes into the open entry; the decided ones are never changed.
   - Refusals: an unknown pattern is `not_found`; a pattern with no open entry (decided already), with two open entries (the curator merges them in its next cycle) or with an entry that lacks a `status`, `decided` or `reason` line is `conflict` and the file stays as it is; so is any call while the curator is at work (decide after its report).
3. An approved entry becomes a skill only through a worker: mcp__makiba__dispatch a `code` task in the project that will hold the skill, whose brief names the skill, the entry, the raw notes to draw on, and that the skill is written as a `SKILL.md` like the others. You never write a skill yourself.

## Check the chain before you call it done

For an approved candidate, before you tell the person the skill is in:

1. The notes the entry names exist in `raw/` or `raw/archive/`, and come from 3 or more different tasks.
2. The entry is `approved` with a date.
3. The pull request that adds `skills/<skill>/SKILL.md` is merged.
4. The skill's trigger matches the entry's `when`.

Name the first that fails, if any.
