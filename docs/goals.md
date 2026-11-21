# Goal ledgers

Work discovery (`src/work_discovery/`) proposes which open Issues to start next. It orders them by
the goal clauses of the project's ledger. Carried from claude-org-ja `registry/goals/README.md`,
without its judging step (Issue 173).

- **Each operator writes the goals.** A ledger is a file on the operator's machine, not in any
  repository.
- **A project with no ledger yields no candidates.** The scan reports it as unset, so write that
  project's ledger.
- **A ledger that cannot be read yields no candidates either.** The scan reports it with the reason
  (the line and what is wrong with it).

## Place

`$MAKIBA_GOALS_DIR/<owner>/<repo>.md`, default `${XDG_CONFIG_HOME:-~/.config}/makiba/goals/<owner>/<repo>.md`
(spec section 2), with `<owner>/<repo>` the GitHub repository in lower case. For
`https://github.com/Example-Org/Example-Repo` that is `example-org/example-repo.md`. A file whose
name differs only in case (`Example-Org/Repo.md`) is reported as an error, not read.

## Format

```markdown
# <any title>

## G1 <heading of the clause>
<text of the clause (optional, any number of lines)>
- unmet if: <a condition under which the clause counts as not met>
- unmet if: <one line or more>

## G2 <next clause>
- unmet if: ...
```

- A clause heading is `## G<number> <heading>`. The numbers run from 1 with no gap and no
  repetition. **A clause higher up comes first.**
- Every clause has at least one `- unmet if:` line (`*` for `-` and any case of `unmet if` work).
- Other headings and text before the first clause are free and not read. A level-1 or level-2
  heading ends the clause above it; a level-3 heading is part of the clause's text. A line that
  starts with three backticks (after spaces) opens or closes a block whose lines are not read; a
  `~~~` fence is not one, and a longer backtick fence closes at the next line of three backticks
  (the carried grammar). A clause heading inside a `~~~` fence is therefore read as a clause, and
  a number it repeats is reported as an error, not skipped.
- A heading that looks like a clause but is not one (`## G1` with no heading, `## G01 x`, `## G1x`,
  `## g2 x`) is an error.

## How candidates are ordered

1. An Issue names the clause it serves itself: under a `## Goal clause` heading, the first list
   line `- G<number> ...` (as in makiba's own Issues: `- G9 It runs its own development`). Only the
   Issue's body is read, not its comments.
2. Candidates are ordered by that clause (those naming `G1` first), and within one clause by the
   carried order (priority label, follow-up of a recent merge, parallelizable when workers are
   free, smaller effort, milestone, most recently updated). The order is made over all candidates
   before the list is cut to its length.
3. A candidate naming no clause comes after those that do, in the carried order. A clause number
   the ledger does not have counts as naming none, and the candidate says so in its signals.
4. With the candidates the scan reports each ledgered project's clauses (heading and unmet-if
   lines), so the secretary can say in conversation which clause a candidate serves.

No model is asked anything, and no Issue text leaves the machine.
