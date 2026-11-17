# Incidents and the tests that guard them

Each row is something that happened in a predecessor. makiba is built so that it cannot happen in the same way, and a test holds that. Goal clause G2 is unmet while any row has no test or a failing one.

The `Test` column is filled by the Issue named in `Guarded by`: that Issue's acceptance test is the row's test. Issue M0-6 adds a check that every row names a test file that exists.

| # | Incident | Cause then | Structure in makiba | Guarded by | Test |
| --- | --- | --- | --- | --- | --- |
| I-1 | 2026-08-18, claude-org-ja: two "dispatch complete" reports for workers that had not started. | The launch steps left no trace: skipped or done, the record looked the same. | `worker_spawn_verified` is written by code, only after the first turn is observed. | M3-6 | `tests/test_spawn_verified.py` |
| I-2 | 2026-09-25, claude-org-ja: a blind Enter on a startup dialog ended the terminal. | The dialog's default line had become "No, exit"; the procedure still said Enter. | Option lines are matched whole; the cursor is moved and the screen read again before one key. | M3-5 | `tests/test_startup_dialog.py` |
| I-3 | 2026-07-30 to 08-19, claude-org-ja: 134 terminal events stayed undelivered for 20 days. | The runbook named `python`, the machine had `python3`; the relay failed on every cycle and nothing said so. | Delivery is a function inside `makibad`. It stops only when the process dies, which the heartbeat shows. | M2-4, M2-1 | `tests/test_delivery.py`, `tests/test_heartbeat.py` |
| I-4 | claude-org-ja #1168: the dispatcher announced its monitoring loop and never armed it. | A model was the loop. | `makibad` is not ready until the base's event stream has given its first full list. | M2-1, M1-3 | `tests/test_heartbeat.py`, `tests/test_events_resync.py` |
| I-5 | claude-org-ja #941: a CI-green notice went to a route nobody read. | Two transports with different defaults. | One route. Terminal events reach the secretary through the delivery ledger. | M2-4 | `tests/test_delivery.py` |
| I-6 | 2026-08-29, claude-org-ja #978: a dead PR watcher looked alive. | The finished watcher's pane stayed open. | `makibad` owns one watch per PR and head and replaces it on a push. | M5-3 | `tests/test_pr_watch.py` |
| I-7 | paddock #334, #388: the secretary's own wait rule was broken or worked around; the per-write nudge never fired on the host. | A model was asked to keep itself awake. | `makibad` delivers. No model waits. | M2-4 | `tests/test_delivery.py` |
| I-8 | paddock #392: a CI watch ended and nobody wrote its result; its exit code was gone. | The script that waited was the only writer, and the child was removed on exit. | `makibad` owns the watch and always records `ci_completed` or `ci_watch_failed`. | M5-3 | `tests/test_pr_watch.py` |
| I-9 | paddock #216: a worker in a new git worktree stopped at the folder-trust dialog. | Trust of a parent folder does not cover a git worktree; the repository's own trust does. | Worktrees are cut from the project's own repository. A dialog that still shows is passed by the policy list. | M3-2, M3-5 | `tests/test_worktree.py`, `tests/test_startup_dialog.py` |
| I-10 | claude-org-ja #1235: after a restart an old event cursor hid a terminal's end. | The cursor outlived the process it belonged to. | A position in the base's event stream is trusted only within one connection; every reconnect takes a fresh list. | M1-3, M2-5 | `tests/test_events_resync.py`, `tests/test_recover.py` |
