# Upgrading the base

makiba runs against one herdr-core release: `>=0.1.21,<0.1.22`, in `makiba/base/version.py`. It is the version the design was read against and the one the probes of [`docs/design/probes.md`](design/probes.md) ran on. `Base.hello()` is checked against it, and anything else raises `UnsupportedVersion` before any other call. `scripts/install-base.sh <prefix>` installs the pinned release (CI and people use it).

herdr-core releases every few days and calls its API a preview. The pin does not follow the releases: a move costs a test run and several probes.

## When the pin moves

- A release carries something makiba needs.
- The pinned release can no longer be installed.
- The owner's monthly look at the base's changelog finds a reason.

Not on every release, and not more than about once a month.

## What a move reruns

On the new version:

1. M1-5's test, `tests/test_base_compat.py`, against the new release's own definition (`hc schema`).
2. The whole suite, on the stand-in and on the real base.
3. Probes P1, P3, P4, P5 and P8. They measure how the base is reached, typed into and launched, and how a turn start shows in its status; a patch release has changed this before (0.1.20 changed sends). Each takes a few hours of the owner's time.
4. P2 reruns itself in CI.

A probe may be skipped only when the changelog shows nothing touching its area, and the record says so. P6 is not rerun.

Then change the number in `makiba/base/version.py`, record each result in probes.md (section 4) with the versions, and update the version named in probes.md and plan.md.

## The range stays one patch release

A wider range would let in a version that no probe has run on. It is revisited only after three pin moves in a row needed nothing but the number changed.
