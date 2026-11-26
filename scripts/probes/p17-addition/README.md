# makiba-p17: a herdr-app addition for probe P17

A trial addition (a herdr-app "mod") for probe P17, "Where the person's screen can run" ([docs/design/probes.md](../../../docs/design/probes.md), script [p17-screen-place.sh](../p17-screen-place.sh)). It is a probe tool, not part of makiba. It adds one panel to herdr-app that runs two fixed `makiba` commands as the person and shows their exit code, stdout and stderr:

- `makiba status --json`, every 5 seconds while refreshing is on (M1);
- `makiba config get conversation_language`, once per click (M2).

It runs nothing else. The two argument vectors are constants in `makiba-p17/main.js`. It never builds a shell line and never retries a failed run: while refreshing is on, each 5-second tick is a new reading, and a tick starts only after the last run ended. It offers no verb that changes state.

## Install

1. Copy the folder `makiba-p17/` into the `mods/` folder of herdr-app's home on the machine where the app runs, so that it becomes `<app home>/mods/makiba-p17/`. The app home is `~/.config/herdr-app`, or `$HERDR_APP_HOME` when that is set (on Windows, the app's home on Windows).
2. Check it: `herdr-app check <app home>/mods/makiba-p17`. It must exit 0.
3. The app docks the panel on the right the first time it sees the folder. To put it beside a terminal instead, focus that terminal (the secretary's) and choose "makiba P17: open beside the focused terminal" in the command palette.

## Reaching the base

The panel's first section says where the two commands run, and shows the machine and the whole argument vector before each run and in each result.

- **Way 1, a herdr machine (`on`).** Pick the machine the base runs on in the "Machine" row and leave the prefix `[]`. The panel then calls the API's `exec(makiba, args, { on: <machine> })`, which herdr-core runs on that machine in its home. When herdr lists the WSL core as a machine (the row shows its name, as `hc machine ls` does on the app's side), pick that name. "this computer" means the machine where the app runs. In the owner's P17 run (2026-11-26), `on` reached the base in WSL, but the `PATH` it ran with did not include the user's `~/.local/bin`, so `makiba` was not found by name. Put the absolute path of `makiba` on the base (`command -v makiba` in a login shell there) in the "makiba program" field.
- **Way 2, a prefix.** Pick "this computer" and set the prefix to a JSON array that reaches the base, for example `["wsl.exe", "-e"]` or `["wsl.exe", "-d", "<distro>", "-e"]` from Windows. `wsl.exe -e` runs the program without a login shell, so `makiba` may not be on its `PATH`: put the absolute path of `makiba` in WSL in the "makiba program" field.

The prefix is only for reaching the base. Do not put any other command in it: everything in the prefix runs before `makiba`.

## Which P17 step uses which control

| P17 steps | Control in the panel | Feeds the result line |
|---|---|---|
| 4 and 5 (M1) | "Start refreshing status --json": the run count, the time, the exit code, `makibad running` and `heartbeat_at` change every 5 seconds | `m1-command` |
| 6 to 8 (M3, M1 by the page) | None. herdr-app's addition API has no web view, so the stand-in page cannot be shown in the app by this addition. "Open in system browser" hands the pasted URL to the system browser, outside the app; it does not count | `m1-page` and `m3-page`: no (no web view) by this addition |
| 9 to 11 (M2) | "Run config get conversation_language": the exit code, the answer on stdout, and stderr. At step 11 the way is `extension` | `m2-act` |
| 12 and 13 (M2 answer) | None: the panel has no `answer` control | `m2-answer` is not served by this addition |

## Remove it afterwards

Close its pane if one is open, then delete `<app home>/mods/makiba-p17/`. The panel keeps its machine, prefix and program choice in the app's `localStorage` for this mod (key `makiba-p17.reach`); that small value may stay in the app's data after the folder is gone, and nothing reads it then.

## What only the person can check

The addition has been checked here for syntax only (`node --check`). Whether the app loads it (`herdr-app check`), whether `on` reaches the WSL base, whether `wsl.exe -e` finds `makiba`, and whether the door records the M2 request as actor `person` can be seen only by running it in the app.
