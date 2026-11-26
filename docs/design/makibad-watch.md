# The watch of makibad (Issue 563)

`makibad` cannot watch itself. This note says what each way of stopping leaves behind, who
notices a stopped or stalled `makibad`, and what the person is told.

## 1. What a stop leaves

`makibad.log` is `makibad`'s stderr and stdout (`makiba up` opens it). Its lines carry no time,
and a healthy `makibad` writes only errors: an idle log is normal. `makibad.lock` holds the
holder's pid and a clean stop empties it.

| Way of stopping | `events` | `dispatcher` row | `makibad.log` |
|---|---|---|---|
| SIGTERM or SIGINT (`makiba down`; the system's TERM while the file system is up) | `dispatcher_stopped` | `subscribed = 0` | nothing |
| `serve_until` returns an error (the subscription ends before its first `Resync`, also by a panic of that thread; a refused stop write) | none | unchanged | `makibad: <error>` |
| A panic of the main thread (the tick) | none | the heartbeat stops | `thread 'main' panicked at ...` |
| A panic of another thread after the subscription | none | the heartbeat goes on: makibad has not stopped | `thread '<name>' panicked at ...` |
| The tick hangs while the process lives | none from the tick | the heartbeat stops; the lock is held | nothing |
| SIGKILL (the OOM killer, `kill -9`), or the machine stops first | none | the heartbeat stops | nothing |

A gap with no row and no log line fits a hang (later killed), SIGKILL or the machine stopping.

The case of 2026-11-26: the WSL instance's journal shows the boot ending with a power-off at
02:28:09Z, five minutes after the last row (02:23Z), and the next boot at 18:05Z. The owner's
Windows System log shows a restart of the PC started at 02:27:57Z on behalf of the signed-in
user. Windows restarted, and WSL and makibad stopped with it.

What tells the ways apart next time:

- The stopped makibad's last heartbeat. Before this change the next start overwrote it. Now the
  start keeps it in `dispatcher_stop_unrecorded` (pid, `started_at`, `heartbeat_at`), written in
  the same transaction that overwrites the row, so a start that fails earlier leaves the row for
  the next one.
- The system journal: a power-off after the last heartbeat is a machine stop; an OOM line is a
  kill; neither, with the lock still held, is a hang.
- Proposed (not built): a time on every log line, and a line when a stop signal arrives.

## 2. Who notices

| What stopped | Who notices | How the person is told |
|---|---|---|
| The tick (makibad lives) | the secretary's channel server: the door's answers carry the heartbeat's age (`X-Makiba-Heartbeat-Age`, to the secretary only) and it reaches 30 s | one `watch` notice to the secretary; `makiba status`'s first line; `makiba up` names the stalled makibad and the way out |
| makibad (killed, crashed) | the secretary's channel server: the door has not answered for 30 s | one `watch` notice to the secretary; `makiba up` names the stop that was not recorded |
| The secretary | makibad (its secretary pass relaunches it) | as before |
| The whole machine (a restart, a power-off) | nobody while it is off | after the restart: `makiba up`'s line "the previous makibad (pid P) stopped without recording it; its last heartbeat was T" and `makiba status`'s first line `makibad is not running: ...` |

The watch is owned by `src/channel.rs`; "stale" (30 s, `status_local::STALE`) by
`src/status_local.rs`. The channel server keeps its watch in memory only (whether it is the
secretary's, since when the door has not answered, whether it gave a notice for this spell); a new
server starts empty. The door answers the first request of a new session of the secretary's
server at once, with the age, so the server knows it watches without waiting out a hold; a server
that never reached the door does not watch. A notice is given once per spell, and again only after a fresh heartbeat.
A silence is counted from the door's last answer. Bounds: a killed makibad within 30 s of that
answer plus the ask's backoff (2 s); a process that keeps its socket open and answers nothing
(stopped by a signal) once the held request's read timeout ends (35 s); a stopped tick within
30 s plus one hold of the door (25 s) and its IO limit. When a stalled makibad holds its lock,
`up` names it before any ledger write (its write lock may be held) and writes and starts nothing. A `makiba down` without `--secretary` leaves the secretary, whose watch
then gives one notice; its text says that is fine after `makiba down`.

## 3. After a restart of the machine

A restart of the PC stops makiba and cannot be avoided; what matters is noticing afterwards.
Today nothing starts on its own when WSL starts: the secretary's terminal is gone or shows a bare
shell, and the person learns of it only by running `makiba up` or `makiba status`, which then say
that makibad stopped without recording it.

Should `up`'s line name the restart? The previous boot's end can be read without privileges where
the journal is persistent and readable by the person (`journalctl --list-boots -o json` gives each
boot's first and last entry; on the owner's machine `/var/log/journal` carries such an ACL). That
is not true of every machine and it needs a `journalctl` child of `up`, so this change keeps the
hint "(see makibad.log and the system journal)". Proposed follow-up: when the previous boot's last
entry is after the last heartbeat, add "the machine restarted at <time>"; say nothing more when
the journal cannot be read.

Proposed follow-up: tell the person at their next sign-in. (a) A check only: the login shell's
profile, or a WSL boot command, runs `makiba status` quietly and prints one line when it finds the
stale line or an unrecorded stop; it starts nothing. (b) Start makiba: `makiba up` at WSL start
through a systemd user unit. (a) is the safer one, since it starts nothing without the person;
the choice is the owner's.

Also not built: running the person-call command (Issue 272) from outside makibad. It would run the
person's command from the secretary's process tree; it needs a process outside every sandbox that
outlives makibad.
