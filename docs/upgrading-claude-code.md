# Upgrading the supported Claude Code versions

makiba supports the Claude Code versions in `SUPPORTED` of [`src/agents/version.rs`](../src/agents/version.rs) (now `>=2.1.326,<=2.1.328`) and checks the text `claude --version` prints against it. It is a separate constant from the base's ([upgrading-the-base.md](upgrading-the-base.md)), because Claude Code is not behind the seam and updates itself. The check at `makiba up` is M4-4's.

The range runs from the lowest to the highest version that a recorded run of the probes covers (the `claude --version` column of `docs/design/probes.md`, section 4 Results), and no wider.

## When the range moves

Claude Code releases more often than the base, so the range moves only after the owner has rerun the following on the new version:

- Probes P1, P3, P4, P5 and P7 to P11 of `docs/design/probes.md` (P7, P8, P9, P10, P11 included).
- paddock's carried probes, listed in probes.md section 1.
- The runbook of M4-5, `docs/runbooks/first-worker.md`, once.
- P6 only when `secretary.remote_control` is on.

The startup-dialog and prompt tests of M3-5 and M6-8 use the test agent and cannot show a change in the real screens. These reruns can.

## How

1. Install the new version on a host that runs the pinned base.
2. Run the reruns above and add their rows to the Results table of probes.md, with the `claude --version` text.
3. Change `SUPPORTED`, `LOWEST` and `HIGHEST` in `src/agents/version.rs` to the lowest and highest version the Results table now covers.

## What a move of the range touches

- The three constants in `src/agents/version.rs`.
- The row for the new version in `docs/design/probes.md`.
- The declared test of Issue 153, `tests/claude_range_153.rs`, when the versions it lists change. It names 2.1.326 to 2.1.328 on purpose and is never edited: once 2.1.329 enters the range it fails, and that failure is the signal to look at it (the owner decides whether it is retired or left as the record of that move).

Nothing else under `tests/` needs an edit. The tests that check the range (`tests/base_version.rs`, `tests/up.rs`, and `tests/range_boundary_154.rs` of Issue 154) build the lowest, the highest, one below and one above from `LOWEST` and `HIGHEST`.

## The move to 2.1.328

The range moved from 2.1.326 to 2.1.328 (Issue 153, 2026-11-20) on a **partial rerun**. This is an exception to the list above, made by the owner because the host was on 2.1.328 and the run of M4-5 (Issue 35) cannot start while `makiba up` refuses that version. It does not change the rule for later moves: a move to a version beyond 2.1.328 asks for the full list above.

Rerun on 2.1.328 (rows in probes.md section 4):

- P1 and P11 (rerun, M4-2), commit 4f95687.
- `door-and-base-reach`: 13 of 13 cases before the role hooks were wired into the launch; after Issue 144, steps 1 to 9 answered as before across two runs that the model's safeguards then stopped, and steps 14 and 15 passed in a partial run.
- `shell-mode-reach`: shell-mode lines are sandboxed under the scrub variable.

Not rerun on 2.1.328:

- Steps 10 to 13 of `door-and-base-reach` since Issue 144 (tracked in Issue 150).
- P3, P4, P5, P7, P8, P9 and P10 as their own probes (P7 to P10 last ran on 2.1.327).
- paddock's carried probes.
- The runbook of M4-5, which Issue 35 itself runs.

Where the rest is tracked: Issue 150 (the reach probe is stopped by the model's safeguards at a different step each run), Issue 147 (following Claude Code versions without a person), Issue 35 (the first real worker).
