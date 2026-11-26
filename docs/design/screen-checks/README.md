# Checks of the screen draft

Three checks of [screen-draft.html](../screen-draft.html) and [screen-fixture/](../screen-fixture/) (Issue 394), run by hand from the repository root. None is wired into CI.

- `wrapcheck.js` with `wrapcheck.sh`: renders the drawing in a headless Chromium (`$CHROME`, or one found on the machine) with the check injected into a copy, and reports any line that holds a single word and any row cut by the bottom of its place. Run: `sh docs/design/screen-checks/wrapcheck.sh`
- `fixcheck.py`: the fixture's `status.json` is what makiba would read back from its `events.json` (the scan, the open anomalies, each range's places, tasks used and item order, the stops), and the scan's left-out list holds one entry per project of each repository, as `triage.rs` writes it (Issue 415: two projects on one repository). Run: `python3 docs/design/screen-checks/fixcheck.py`
- `contrast.py`: every colour pair the drawing uses, from its light and dark tokens, against WCAG 2 (4.5:1 for text, 3:1 for marks and focus). Run: `python3 docs/design/screen-checks/contrast.py`

Each exits 0 when all passes and 1 when something does not (`wrapcheck.sh` 2 when it finds no browser).
