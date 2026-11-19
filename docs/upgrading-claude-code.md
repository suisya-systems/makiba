# Claude Code versions

makiba has no upper bound on Claude Code's version. `makiba up` reads the text `claude --version` prints and checks it against the constants of [`src/agents/version.rs`](../src/agents/version.rs):

- `LOWEST` (`SUPPORTED`, now `>=2.1.326`): a version below it is refused.
- `REFUSED`: versions refused by name, each with a one-line reason (empty today). `makiba up` refuses a listed version and prints the reason.
- `NEWEST_CHECKED` (now 2.1.328): the newest version the organisation has run the probes on. It is information, not a bound: when the version found is newer, `makiba up` prints one line saying so and goes on.
- A text that cannot be read as a version is refused.

The reason for no upper bound: Claude Code updates itself, several times a week, and a user cannot choose its version, unlike the base's ([upgrading-the-base.md](upgrading-the-base.md)), whose release makiba installs. What an upper bound would catch is mostly caught already at launch: a startup dialog that is not on the list fails the launch with the screen recorded, and a launch that does not start working fails its verification. What it alone would catch is a version whose sandbox settings silently mean less. That is guarded where it stops nobody: the organisation runs the probes on new versions, and a bad one is refused by name.

## A person who installs makiba

Does nothing. Claude Code may update at any time; `makiba up` accepts it.

## When a new version appears

The organisation, on a host that runs the pinned base:

1. Run the reach probe in shell mode (`PROBE_MODE=shell`), then the default-mode steps that its README names for what shell mode does not cover ([`scripts/probes/README.md`](../scripts/probes/README.md): the Read and Edit tool steps, and the steps it names for `unsandboxed-run`).
2. Record the result as a row in the Results table of [`docs/design/probes.md`](design/probes.md) (section 4), with the `claude --version` text. When the probes of that file are rerun as well (P1, P3, P4, P5, P7, P8, P9, P10, P11, and P6 only when `secretary.remote_control` is on; paddock's carried probes are listed in probes.md section 1), their rows go there too. The first-worker runbook, `docs/runbooks/first-worker.md` (M4-5), is run once when a person judges that the change calls for it.
3. Move `NEWEST_CHECKED` in `src/agents/version.rs` to the version. No test needs an edit: the tests build their versions from the constants.

Until step 3, `makiba up` prints its one line on that version and starts as usual.

## A bad version

When a run finds a version whose sandbox or launch is worse than the last checked one, add it to `REFUSED` with a one-line reason that says what was found and where it is recorded (the Results row or the Issue). `makiba up` then refuses that version with the reason. Remove the entry when a later run shows the version fine, or leave it when the version is gone for good. Raising `LOWEST` is the same kind of change, for versions the organisation no longer supports.

The tests of the refused list pass their own list to `check_with` and never add an entry to the shipped one, so adding a real entry needs no test edit.

## History: the move to 2.1.328

The range moved from 2.1.326 to 2.1.328 (Issue 153, 2026-11-20) on a partial rerun, because the host was on 2.1.328 and the run of M4-5 (Issue 35) could not start while `makiba up` refused it. Rerun on 2.1.328 (rows in probes.md section 4): P1 and P11 (M4-2, commit 4f95687); `door-and-base-reach` (13 of 13 cases before the role hooks were wired into the launch; steps 1 to 9 as before and steps 14 and 15 after Issue 144; steps 10 to 13 are tracked in Issue 150); `shell-mode-reach`. Not rerun: P3, P4, P5, P7 to P10 as their own probes, paddock's carried probes, and the first-worker runbook.

Issue 160 (2026-11-20) replaced the upper bound with this rule; it also closed Issue 147, whose automatic check was not built.
