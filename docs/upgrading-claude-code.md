# Upgrading the supported Claude Code versions

makiba supports the Claude Code versions in `SUPPORTED` of [`src/agents/version.rs`](../src/agents/version.rs) (now `>=2.1.326,<=2.1.326`) and checks the text `claude --version` prints against it. It is a separate constant from the base's ([upgrading-the-base.md](upgrading-the-base.md)), because Claude Code is not behind the seam and updates itself. The check at `makiba up` is M4-4's.

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
3. Change `SUPPORTED`, `LOWEST` and `HIGHEST` in `src/agents/version.rs` to the lowest and highest version the Results table now covers, and the cases of `tests/base_version.rs` that name them.
