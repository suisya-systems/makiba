# makiba: handoff bundle

Everything needed to start building makiba with claude-org-ja. Written 2026-11-18 after reading
claude-org-ja `main` (#1279), paddock `main` (#468), claude-org-runtime 0.1.58, herdr-core 0.1.21,
Orca `main` of 2026-11-17 and the Claude Agent SDK documentation of the same day.

makiba has one requirement: the person deals with one secretary and nothing else (`plan.md`
section 1). It is judged by one measure: the discipline and the design of the mechanism are
makiba's own, and nobody but the model provider shapes how the work is done (`plan.md`, "The
measure"). From those two follow the choices that were open in the earlier bundle and are now
made:

- The secretary and the workers are interactive Claude Code sessions in terminals. Not `claude -p`,
  not the Agent SDK.
- The terminals run on herdr-core, behind one module. Orca's low-level commands are the second
  choice for that module and are not built. Orca's orchestration is not used.
- The two Issues that tried Orca first (the earlier M0-9 and M0-10) are gone. Work starts at M0-1.

| Path | What it is |
| --- | --- |
| `docs/design/plan.md` | The measure, the requirement, where this is going, the base and how agents run, scope and what the predecessors become, decisions, the flow of one task, what is carried, milestones, how it is developed and what limits the schedule. |
| `docs/design/spec.md` | Names and shapes shared across Issues: tables, the base interface, tags, the door, the message wrapper, event kinds, commands. |
| `docs/design/incidents.md` | Ten past incidents and the test that guards each. |
| `docs/design/probes.md` | What paddock measured that makiba relies on, the first host measurements (2026-11-18) and the measurements still to make, and the checks to run if a move to Orca is ever considered. |
| `issues/` | 51 Issue bodies and `index.md`. Generated from `tools/build_issues.py`. |
| `goals/suisya-systems/makiba.md` | The goal ledger, checked with claude-org-ja's own validator. |
| `tools/create_issues.py` | Creates the Issues with `gh` in dependency order and fills in the numbers. |

## Order of use

1. Create the repository `suisya-systems/makiba` and commit `docs/`, `issues/`, `tools/`. Create
   the labels `M0` to `M7`. Dry run, then create the Issues:

   ```sh
   python3 tools/create_issues.py --repo suisya-systems/makiba
   python3 tools/create_issues.py --repo suisya-systems/makiba --apply
   ```

2. In claude-org-ja: register the project, and copy the ledger to
   `registry/goals/suisya-systems/makiba.md` (that folder is not tracked by git).
3. Upgrade herdr-core on the host to the version M0-2 pins (0.1.21; a host that still has 0.1.18
   must not run the probes on it). Probes P1, P3, P4 and P5, and parts of P8 to P11, were run on 2026-11-18 and are recorded in
   `docs/design/probes.md` section 4; the owner's decisions on the door and on delivery that followed are in
   `plan.md` section 3.3. Still to run, from a login shell, each recorded in the same place: P2 (where
   real-base tests run; M0-3 waits for it), P7 (whether your own Claude Code settings reach the agents; M3-4,
   M4-3), and the steps of P8 to P11 that the record says were not run (M2-4, M3-3, M3-5, M6-5; the box and the wake line are M2-7). They need no
   makiba code. Tell the secretary that no Issue of M2 or M3 is dispatched until every one of P1 to P5 and
   P7 to P11 has a row there.
   P6 waits for M6-2.
4. Start with M0-1. It is merged in `human` mode (its own acceptance job cannot pass); set branch
   protection after it lands. From M0-2 on, `merge_approval_mode` is `acceptance`.

## Before trusting this bundle

- The Issue bodies name files in the predecessors. Every named path was checked to exist at the
  commits above; what is inside each file was read for some and only listed for others.
- There is no calendar estimate. What limits the schedule is the owner-run probes, the
  owner-checked Issues (M4-5, M7) and the serial chains in the dependency graph, counted in
  `plan.md` section 9.
- The choice of herdr-core over Orca rests on the measure and on what was read in Orca's
  repository (the CLI and orchestration guides, the status integration README, the notification
  code). Orca was not run. What Orca does better, and what would reopen the choice, is in
  `plan.md` section 3.1.
- The choice of interactive Claude Code over the Agent SDK rests on the SDK's documentation and
  on public issue reports about connectors in non-interactive runs. Neither was reproduced here,
  and the two ways of running were not compared on a real task.
- Reaching the secretary from a phone through Claude Code's Remote Control was read in its
  documentation, not run (probe P6). The plan does not depend on it.
- `spec.md` was checked this far: its SQL runs on top of the carried schema in SQLite, and every
  test file named in `incidents.md` is the acceptance test of an Issue. Nothing in it has been built.
- Time values, limits and defaults marked "first proposal" are exactly that.
