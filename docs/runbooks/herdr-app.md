# Runbook: seeing the organization in herdr-app (Issue 196)

herdr-app shows a terminal when two things hold (measured, [probes.md](../design/probes.md) P12):

1. The app dials the base's session. `makiba up` starts a core of its own session (`makiba`, or `MAKIBA_BASE_SESSION`). The app's machine for WSL is bound to WSL's default session and never dials it, so the session is registered on the app's side once.
2. The terminal carries the labels `herdr.space`, `herdr.tab` and `where`. Every terminal `makibad` launches carries them (spec section 6.3): one space per organization, one tab per terminal.

## 1. Register the session (once per session name)

`makiba up` prints the line, with the session's name, after it starts the base:

```
app: to see this organization in the app, run on the app's machine: hc.exe machine add wsl --name <session> --session <session>
```

Run that command on the machine where herdr-app runs (for herdr-app on Windows with the base in WSL, in a Windows shell). `makiba up` never runs it and never writes the app's machine list: that list is the person's. `makiba status` does not show whether the session is registered: the list is on the app's side, and makiba cannot read it without running a program there.

Check on the app's machine:

```
hc.exe machine ls
```

The machine named after the session is listed and `connected`. `hc.exe ls` lists the organization's terminals as `<session>/<terminal>`.

To undo: `hc.exe machine rm <session>`.

## 2. Terminals started before the labels

A terminal launched by a `makibad` older than Issue 196 carries no labels and stays out of the app. Relaunch it: `makiba down --secretary`, then `makiba up`. The secretary and the suspended workers are launched again, with the labels.

## 3. What the app shows

Under the machine named after the session, one space for the organization (the app names it after a terminal's working folder), one tab per terminal (named after the program the terminal reports), and each agent's state in its Agents list.

## 4. Host record

The check that the app shows a real organization (a secretary, a worker and the curator) is run once on a host after Issue 196 lands, by the secretary with the person watching the app. Record it here:

| Date | `hc --version` | herdr-app version | Session | Seen in the app | Notes |
| --- | --- | --- | --- | --- | --- |
| | | | | not yet run | |
