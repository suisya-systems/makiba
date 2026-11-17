# Upgrading Claude Code

makiba accepts the Claude Code versions in `makiba/agents/version.py`: from the lowest to the highest version that a recorded run of the probes covers (the `claude --version` column of Results in [`docs/design/probes.md`](design/probes.md)), and no wider. Today that is 2.1.326, the version of the first recorded runs. The text `claude --version` prints is checked against it, and a version outside it, or text that cannot be parsed, is refused with the version found and the range.

It is a separate constant from the base's ([`upgrading-the-base.md`](upgrading-the-base.md)): Claude Code is not behind the seam and updates itself. The check at `makiba up` is M4-4's.

## When the range moves

Claude Code releases more often than the base, so the range moves only after the owner has rerun the following on the new version:

- Probes P1, P3, P4, P5, P7, P8, P9, P10 and P11.
- paddock's carried probes (probes.md section 1).
- The runbook of M4-5, `docs/runbooks/first-worker.md`, once.
- P6, only when `secretary.remote_control` is on.

The startup-dialog and prompt tests of M3-5 and M6-8 use the test agent and cannot show a change in the real screens; these reruns can.

Then record the results in probes.md (section 4) with both versions, and change the numbers in `makiba/agents/version.py`.
