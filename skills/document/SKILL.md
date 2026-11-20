---
name: document
description: Run a document request. A maker worker writes the document; a second worker reads the draft against the request and checks that every fact carries a source; only then does the result go to the person.
when_to_use: The person asks for a document (a report, a summary, a comparison, a write-up) rather than a change to a repository.
---

# document

The rules are borrowed from libretto; none of its code or services is used. A document worker has a working folder, not a git worktree: what it hands over goes in `out/`, which is kept after the close (spec section 12).

## 1. The maker

mcp__makiba__dispatch with `kind` `document`, a `task_id`, a `title` and a `brief`. No `project` is needed. The brief says, in English:

- The request, in the person's words translated faithfully, and who will read the document.
- The language of the document.
- Every fact carries its source next to it: a link, a file path with its line, or the name of a document and its section. A claim with no source is marked as the maker's judgment.
- The result goes in `out/` of its working folder. Its done report gives the absolute path of each file in `out/` and lists the sources it used.
- This is not a code task: it has no repository, so it does not commit, and its done report names files instead of a commit. Its rules about the worktree read as its working folder.

## 2. The reviewer

When the maker reports done, do not close it yet. mcp__makiba__dispatch a second `document` task (a new `task_id`) whose brief gives:

- The request, word for word as the maker got it.
- The absolute path of the maker's `out/`. The reviewer reads it and does not edit it.
- Its job: read the draft against the request (what is asked and not answered, what is answered and not asked), and check that each fact carries a source and that the source says it. Report done with a list of findings, each with where in the draft and why, or "no findings".

## 3. Until it is clean

- Findings go to the maker with mcp__makiba__send. When it reports done again, give the reviewer the new draft with mcp__makiba__send and wait for its report.
- Repeat until the reviewer reports no findings. If the two disagree on something only the person can decide, put it to the person (the escalation skill).

## 4. Hand it over

Only now tell the person the result: the path of each file in the maker's `out/`, a short summary, and anything left open. Then save a raw note if either worker's report had one (the curate skill), run the retro skill, and close both workers with mcp__makiba__close. `out/` stays after the close.
