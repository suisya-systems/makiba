---
name: issue
description: Open a GitHub issue with gh from fixed sections and a goal clause, with its dependencies written as one "Depends on #N #M" line at the end of the body.
when_to_use: A decision, a piece of work or a follow-up needs an issue in a repository; you would otherwise write an issue body freely.
---

# issue

The triage scan reads dependencies only from the same line as the keyword (`Blocked by`, `Depends on`, `Requires`); references under a heading are read as no dependency. A line that starts with one of those keywords and names no Issue number ("Depends on the belt's step 2.") leaves the issue out of the range and the scan as `dependency_unreadable`. So every issue you open ends with one dependency line, or none.

## Steps

1. The repository is always named as `<owner>/<name>`; never guess it. Ask the person when the request does not say.
2. Pick the kind and write the sections, in the project's artifact language (English unless your launch task or a later notice names another for the project; spec section 13):

| Kind | Headings, in order |
| --- | --- |
| decide | Question, What we know (with sources), Options, Definition of done, Goal clause |
| build | Why, Definition of done, Goal clause |
| followup | What was seen, Ask, Goal clause |

   The goal clause is one line: the clause number and its heading, from the project's goal ledger.
   A followup also carries, each on a line of its own before the dependency line: `Follow-up of #N` (the Issue it was found on), `Needs host: <host>, <host>` when its work needs a network host, and `Stop line: <line>` when it would hit a stop line (`decision`, `irreversible`, ...). A range whose follow-ups are inside takes a followup by itself unless it needs a host the project does not list or names a stop line: then it is asked for.
3. Dependencies: check each number exists in that repository (`gh issue view <N> --repo <owner>/<name>`); a number that does not exist is a mistake to fix, not to drop. Give each number once. If there is any, the last line of the body is `Depends on #N #M`, alone, on one line.
4. Write the body to a file in `$TMPDIR` and run `gh issue create --repo <owner>/<name> --title "<title>" --body-file <file>`. It prints the issue's link: use that.

If `gh` is not there or the create fails, tell the person the reason; nothing was created.

A repository may be public. An issue carries no operator-private content (the secretary's rules list it), and not the details of a defect that a dependency (the base, the agent program, a reader) has not fixed yet: whether a text is such a detail is judged by its meaning, before you write it, and such a defect goes to the person.
