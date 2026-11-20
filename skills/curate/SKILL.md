---
name: curate
description: Keep a worker's raw lesson note in the knowledge folder, read the curator's report, put pending skill candidates to the person, record the decision and have an approved candidate made into a skill.
when_to_use: A worker's done report ends with a note under the four headings; the curator's report arrives; skill candidates are pending; the person decides a candidate.
---

# curate

Growth through skills: workers' lessons become raw notes, the curator merges them into patterns and proposes candidates, the person decides, and only an approved candidate becomes a skill.

The knowledge folder is `$MAKIBA_KNOWLEDGE_DIR`, by default `~/makiba-knowledge` (spec section 2): `raw/`, `raw/archive/`, `curated/`, `skill-candidates.md`.

**Precondition:** your sandbox must let you write in the knowledge folder (Issue 183). Until it does, give the person the note and the path to save it at.

## A raw note

A worker may end its done report with a note under the four headings `## Facts`, `## Judgment`, `## Grounds`, `## Where it applies`. A report with no note needs nothing. When it has one, save it **before** you close that worker: `makibad` counts the raw notes when a close is recorded, so a note written after the close is not counted until the next one.

- Path: `<knowledge>/raw/<YYYY-MM-DD>-<topic>.md`, `<topic>` in English kebab-case.
- First line `Task: <task_id>`, then a blank line, then the note as the worker wrote it. The curator counts different tasks by this line.
- All four headings must be there; a note missing one goes back to the worker (mcp__makiba__send), not into `raw/`.
- Never overwrite: if the name is taken in `raw/` or `raw/archive/`, add `-2`, `-3`, and so on.
- Never edit a note once saved.

## The curator

`makibad` launches the curator when 5 or more raw notes wait (Issue 39); you do not start it. It does one cycle and reports to you once, with mcp__makiba__send, then ends. Its report says how many notes it read, which patterns are now candidates and which are close. Nothing in it goes to the person as the person's word.

## Candidates

An entry of `skill-candidates.md` has the lines `pattern`, `date`, `notes`, `when`, `skill`, `status`, `decided` and `reason`. Entries with status `pending`:

1. Put them to the person in a batch, in the conversation language, each with its one-sentence `when` and the notes behind it.
2. Write the person's decision into the entry yourself: `status` `approved` or `rejected` (or `merged-into-<skill>` when they say to fold it into an existing skill), `decided` the date, `reason` their words. A pattern already in the file, whatever its status, is not proposed again.
3. An approved entry becomes a skill only through a worker: mcp__makiba__dispatch a `code` task in the project that will hold the skill, whose brief names the skill, the entry, the raw notes to draw on, and that the skill is written as a `SKILL.md` like the others. You never write a skill yourself.

## Check the chain before you call it done

For an approved candidate, before you tell the person the skill is in:

1. The notes the entry names exist in `raw/` or `raw/archive/`, and come from 3 or more different tasks.
2. The entry is `approved` with a date.
3. The pull request that adds `skills/<skill>/SKILL.md` is merged.
4. The skill's trigger matches the entry's `when`.

Name the first that fails, if any.
