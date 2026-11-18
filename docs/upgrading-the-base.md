# Upgrading the base

The base is herdr-core. makiba runs against one release of it and refuses any other. The pin is `PINNED` and `SUPPORTED` in [`src/base/version.rs`](../src/base/version.rs) (`>=0.1.21,<0.1.22`: one patch release, the one the design was read against and the probes of [`docs/design/probes.md`](design/probes.md) were run on). [`scripts/install-base.sh`](../scripts/install-base.sh) installs that release into a prefix and reads `PINNED` from the same file, so the pin has one place.

herdr-core releases every few days and calls its API a preview. The pin does not follow the releases: a move costs a test run and several probes, so it moves on purpose and rarely.

## When the pin moves

Only for one of these reasons:

1. A release carries something makiba needs.
2. The pinned release can no longer be installed (the manifest or the file is gone).
3. At the owner's monthly look at the base's changelog.

Not on every release, and not more than about once a month.

## What a move reruns

On the new release, before the pin is changed:

1. M1-5's test, `tests/base_compat.rs`, against the new release's own definition: save `hc schema` of the new release to a file and run `MAKIBA_BASE_SCHEMA=<file> cargo test --test base_compat`. It names every method, parameter or result field of [`src/base/uses.rs`](../src/base/uses.rs) that the new definition lacks or has changed.
2. The whole test suite, on the stand-in and on the real base (`MAKIBA_TEST_REAL_BASE=1`).
3. Probes P1, P3, P4, P5 and P8 of `docs/design/probes.md`. They measure how the base is reached (P1), typed into (P3, P4), launched (P5) and how a turn start shows in its status (P8). A patch release has changed these before: 0.1.20 changed sends. Each takes a few hours of the owner's time.
4. P2 reruns itself in CI.

A probe may be skipped only when the changelog shows nothing touching its area, and the record in probes.md Results says so. P6 is not rerun.

Then replace [`docs/base/hc.openrpc.json`](base/hc.openrpc.json) with that definition, change `PINNED` and `SUPPORTED` together, extend the Results table of probes.md with the new rows, and update the version named in the rules at the top of probes.md.

## The range stays one patch release

A wider range would let in a version that no probe has run on. It is revisited only after three pin moves in a row needed nothing but the number changed.

## Claude Code

Claude Code has its own range and its own document: [upgrading-claude-code.md](upgrading-claude-code.md).
