# What was not moved

Status: decided by the owner on 2026-11-22 (Issue #53, M7-4). The owner read all thirty marks and kept each as the writer recommended it. The marks below are decisions, not recommendations.

claude-org-ja is stopped at M7-3 ([plan.md](plan.md) section 4.1). This list is every feature of it that makiba does not have, so that each gets a decision and none is lost in silence.

## How to read the marks

| Mark | Meaning |
| --- | --- |
| build | makiba should have it. It is not planned in any milestone, or is planned only as a line in a design note. |
| drop | makiba should not have it. The reason says what makes it unnecessary or harmful here. |
| later | makiba should have it, but not before a named stage. The stage, or the open Issue that already holds it, is written. Each build and later entry names its Issue. |

- The mark on each entry is the owner's decision of 2026-11-22, with the writer's one line of reason. Each build and later entry names the Issue that holds it.
- A later change of a mark is edited in place, and the date of the change is added to the entry.
- paddock is not on this list. It continues as a lightweight organization beside makiba ([plan.md](plan.md) section 4.1). A file makiba copied from paddock is makiba's own from then on and is not a feature left behind. No entry marks paddock or any feature of it as dropped. rondo, cadenza and continuo are frozen by the owner's decision and are not claude-org-ja features; a claude-org-ja item that only serves rondo is marked on its own merits.
- Sources read, all in `/home/happy_ryo/work/org/claude-org-ja`: `README.md`, `CLAUDE.md`, `docs/non-goals.md`, `.claude/skills/*/SKILL.md` (26 skills), `.claude/rules/`, `.dispatcher/` and its `references/`, `.hooks/`, `tools/` (about 120 files), `dashboard/`, `docker/`, `docs/design/`, `docs/contracts/`, `docs/operations/`, `registry/`, `scripts/`. Against them: makiba's `docs/design/` (plan, spec, belt, lines, incidents), `roles/`, `skills/`, `src/` and `issues/`.

Totals: 30 entries. build 3, drop 18, later 9 (counted at the end). The appendix lists what makiba has.

## Entries

An entry is: what it does for the person; where it lives in claude-org-ja; what makiba has nearest to it; the mark and the reason.

### A. What the person sees and is told

**N1. The dashboard.**
- Does: a browser board of the organization (projects, runs, workers, pending decisions), refreshed on request.
- In claude-org-ja: `dashboard/` (`server.py`, `app.js`, `index.html`, `org_state_converter.py`), `.claude/skills/org-dashboard/`, `tools/stop_dashboard.py`.
- Nearest in makiba: the read surface, `makiba status --json` and `makiba events --json` (spec section 12), built so that a screen can sit on it without reading the tables.
- Mark: **later**, Issue #169, the screen (belt.md section 8, step 5).
- Reason: wanted by the person, but only as a read over the read surface; claude-org-ja's board read `.state` files that makiba does not write.

**N2. The cockpit.**
- Does: a tmux window layout with a status line that lists "Needs you" for the secretary and the workers waiting on a decision.
- In claude-org-ja: `tools/org_cockpit.py`, `tools/cockpit_state.py`, `tools/org-dispatcher-view.sh`, `.hooks/mark-needs-decision.sh`, `.hooks/clear-needs-decision.sh`, `docs/operations/dispatcher-view.md`, `renga-layouts/ops.toml`.
- Nearest in makiba: the pending table (`makiba pending list`) and the screen of #169, which shows the same "waiting for you" list.
- Mark: **drop**.
- Reason: it exists only for the tmux broker path, and its job (what waits for the person) is the pending list that #169 shows; a window layout over terminals runs against "the person is never called to a terminal".

**N3. Attention notifications.**
- Does: an OS notification and a sound when a worker needs the person (approval, decision, CI failed, done), so the person is called back without watching.
- In claude-org-ja: `docs/design/attention-notification.md`, `.claude/skills/org-attention-start/`, `.claude/skills/org-attention-stop/`, `tools/templates/attention.example.json`, `docs/operations/attention-watch.md`, `tools/secretary_queue_watcher.py`.
- Nearest in makiba: none. plan.md section 4 lists "notifications" as out, "each a later plan". The base sends no notices of its own, so the contact stays the secretary.
- Mark: **later**, after #169; Issue #272.
- Reason: wanted, but it must be fed by `makibad`'s pending entries and sent as the secretary's call, not as a worker's, to keep one contact; that needs its own design.

**N4. Metrics report.**
- Does: a read-only report of runs, events, PRs and CI over a period, as Markdown or JSON, for the team and the owner.
- In claude-org-ja: `tools/org_metrics_report.py`.
- Nearest in makiba: `makiba events --json` holds the rows a report would read.
- Mark: **later**, once the read surface of #168 and the screen #169 have run for a while; Issue #273.
- Reason: it is a pure read over events, so it can wait until there are weeks of makiba's own events to report on.

**N5. Defense report and the defense register.**
- Does: counts how often each registered defense stopped something, so a defense that never fires can be questioned.
- In claude-org-ja: `docs/operations/defense-register.md`, `tools/defense_report.py`.
- Nearest in makiba: `docs/design/lines.md` (what holds each line) and `docs/design/incidents.md` (the incident for each test). They are a register without a count.
- Mark: **later**, with N4; Issue #274.
- Reason: the count needs an event for each stop; whether makiba's hooks record one is not yet checked.

**N6. Direct attach to a pane.**
- Does: prints the command that opens a live terminal of the secretary, the dispatcher or a worker.
- In claude-org-ja: `.claude/skills/org-attach/`.
- Nearest in makiba: `makiba peek <task_id>`, a read of the last lines of a worker's screen. The base has its own window (`docs/runbooks/herdr-app.md`).
- Mark: **drop**.
- Reason: it is tmux-specific, and a way into a worker's terminal is the thing the requirement removes; `peek` and the base's own window serve debugging.

### B. Starting, stopping, handing over

**N7. Automatic handover of the secretary at a context threshold.**
- Does: a watcher notices that the secretary's (and the dispatcher's) context is large, and runs handover, `/clear` and resume without being asked.
- In claude-org-ja: `docs/design/context-auto-handover.md`, `tools/context_handover_watch.py`, `tools/context_record.py`, `.hooks/context-record.sh`.
- Nearest in makiba: `makiba renew --handover-file` (M6-5), which the secretary calls itself, and `makibad` launches a fresh secretary without typing into it.
- Mark: **later**, after M7-2 shows how often the secretary fills up; Issue #275.
- Reason: the manual renewal exists; whether to automate it is a measurement the week of self-hosting gives.

**N8. Handover and resume of the dispatcher.**
- Does: the model dispatcher writes its monitoring state to a file, is cleared, and resumes its `/loop`.
- In claude-org-ja: `.claude/skills/dispatcher-handover/`, `.claude/skills/dispatcher-resume/`.
- Nearest in makiba: `makibad` is code; its state is `state.db`, and `up` rebuilds the view from labels (M2-5).
- Mark: **drop**.
- Reason: there is no model dispatcher to hand over.

**N9. Interactive start briefing and setup.**
- Does: `/org-start` loads the last state, briefs the person, and starts the dispatcher pane; `/org-setup` places permission files for every role and `--prune` rewrites stale ones.
- In claude-org-ja: `.claude/skills/org-start/`, `.claude/skills/org-setup/`, `tools/org_setup_prune.py`, `scripts/install.sh`, `scripts/install.ps1`, `scripts/install-hooks.sh`.
- Nearest in makiba: `makiba up`, the binary's role files and per-task settings written at launch; the install is one binary (#62).
- Mark: **drop** for the skills and `--prune`; the installer is carried by #62.
- Reason: settings are generated per launch from the binary, so there is nothing stale to prune and no setup dialogue.

**N10. The worker brief generator.**
- Does: builds a worker's `CLAUDE.md` or `CLAUDE.local.md` from a TOML file and templates, in a normal and a self-edit variant.
- In claude-org-ja: `tools/gen_worker_brief.py`, `tools/gen_delegate_payload.py`, `tools/templates/worker_brief*.md`, `tools/templates/worker_brief.example.toml`.
- Nearest in makiba: `makiba dispatch --brief-file`; the worker's rules are `roles/worker/rules.md`, passed at launch and outside the worktree.
- Mark: **drop**.
- Reason: nothing is written into a target repository, so a brief file in the worktree has no place; the task text is the brief.

### C. Who may do what, and what is checked

**N11. Self-edit approval of `.claude/**`.**
- Does: a one-command handshake by which the person approves a worker that edits the organization's own rules, and a hook that blocks edits of the organization's structure otherwise.
- In claude-org-ja: `tools/self_edit_approval.py`, `.hooks/block-org-structure.sh`, `.claude/skills/org-delegate/references/claude-org-self-edit.md`.
- Nearest in makiba: role rules live outside the worktree and are passed at launch, and a worker may not edit its own settings (L-K5).
- Mark: **drop**.
- Reason: in makiba the rules are source files in the repository, changed by an ordinary pull request that passes the gate, while the running rules are the installed binary's copy; no separate handshake is needed. Reconsider if a worker's edit can reach the installed copy.
- Checked facts (writer's reading, 2026-11-22): the running rules are files at `<share>/roles/<role>/rules.md`, rewritten from the binary's embedded copy at every launch (`src/agents/mod.rs`, `write_rules`; called from `src/daemon/prepare.rs`); a worker's settings and its sandbox deny writes to the share folder (`roles/worker/settings.json`, `src/daemon/prepare.rs`). Open point: whether anything reinstalls the binary after a landing without a person; if something does, a merged change to `roles/` goes live with no person, and the mark should be looked at again.

**N12. Checks that the model dispatcher ran its steps.**
- Does: deterministic checks that the dispatcher followed its own prose: the spawn-completion gate, the retro-completion gate, the loop-directive guards, blocking a close before retro, the ad hoc PR watch block, cursor reset and the restart guard of a watcher.
- In claude-org-ja: `tools/spawn_gate.py`, `tools/dispatcher_retro_gate.py`, `tools/dispatcher_loop_watch.py`, `tools/cursor_reset.py`, `tools/watcher_restart_guard.py`, `tools/inspect_anomaly_scan.py`, `.hooks/check-dispatcher-loop.sh`, `.hooks/check-loop-directive.sh`, `.hooks/block-close-before-retro.sh`, `.hooks/block-adhoc-pr-watch.sh`, `.hooks/block-dispatcher-out-of-scope.sh`, `.dispatcher/references/loop-directive-guard.md`.
- Nearest in makiba: `makibad` does these steps itself (launch verified by an observed first turn, M3-6; close refused before a done report, M5-1; one PR watch per head, M5-3). plan.md section 7 lists "the tools that check whether the model ran a step" as not carried.
- Mark: **drop**.
- Reason: the dispatcher is code, so nothing needs to check that a model did what it was told.

**N13. Verify the person's premise before agreeing.**
- Does: a hook reminds the secretary to open the record before agreeing with a premise the person states (Issue #1192 of claude-org-ja).
- In claude-org-ja: `tools/premise_check.py`, `.hooks/remind-verify-premise.sh`.
- Nearest in makiba: none found in `roles/secretary/rules.md`.
- Mark: **build**, as one rule in the secretary's rules; Issue #269.
- Reason: the incident that made it (agreeing without opening a record already read) is about the secretary, which makiba still has, and a rule costs a paragraph.

**N14. The capability gate for a new backend.**
- Does: records whether a renga 2.0 capability gate has been passed, before work depending on it starts.
- In claude-org-ja: `tools/capability_gate.py`, `docs/design/capability-first-drive-gate-maintenance.md`, `docs/contracts/backend-interface-contract.md`.
- Nearest in makiba: the probes P1 to P11 and the version pin (M0-2), which hold the same idea for the base and Claude Code.
- Mark: **drop**.
- Reason: it is about renga's backends; makiba has one base and a probe for each assumption.

**N15. Contracts of the transport and the launcher.**
- Does: written agreements for the backend interface, the sandbox launcher, role patterns in the sandbox, state schema, fixture scrubbing, and curation.
- In claude-org-ja: `docs/contracts/backend-interface-contract.md`, `sandbox-launcher-contract.md`, `role-pattern-sandbox-contract.md`, `state-schema-contract.md`, `state-fixture-scrub-policy.md`, `knowledge-curation-contract.md`, `worker-git-guardrails-design.md`.
- Nearest in makiba: `docs/contracts/` holds the two that were carried (state semantics, delegation lifecycle); `roles/`, `lines.md` and `spec.md` hold the rest in makiba's terms.
- Mark: **drop**.
- Reason: they describe the renga and tmux layers and the old state files. One exception to check: `knowledge-curation-contract.md` may hold rules the curator skill lacks; it is read in the pass of N18.

### D. Knowledge, retrospectives and skills

**N16. Skill eligibility check and skill audit.**
- Does: decides whether a work pattern becomes a skill (recommended, candidate only, or a note) and, when the queue is long, audits skills for retirement, duplicates and owners.
- In claude-org-ja: `.claude/skills/skill-eligibility-check/`, `.claude/skills/skill-audit/`, `knowledge/skill-candidates.md`.
- Nearest in makiba: the curator's rules and `skills/curate/SKILL.md` record candidates, and `makiba candidate` records the person's decision (Issue #191). The eligibility judgment and the audit are not written.
- Mark: **later**, when the candidate queue or the skill count is large enough to need it (claude-org-ja's own thresholds were 5 pending and 20 skills); Issue #276.
- Reason: makiba has none of either yet; the queue exists, the volume does not.

**N17. Creating a work-skill from an approved pattern.**
- Does: when a completed task's pattern is judged worth keeping, a skill is written under `.claude/skills/` so the next worker starts with it.
- In claude-org-ja: `.claude/skills/org-retro/` (the work-skill decision), `.claude/skills/skill-eligibility-check/`, work-skills under `.claude/skills/`.
- Nearest in makiba: `makiba candidate <pattern> into-<skill>` marks that a pattern goes into a skill; the skill itself is written by hand in a pull request.
- Mark: **later**, after N16; Issue #277.
- Reason: a skill is code that every later worker reads, so it goes through a pull request; automation can follow once candidates are approved reliably.

**N18. Knowledge notes already written.**
- Does: the curated and raw notes of what claude-org-ja learned (codex behaviour, sandbox, delegation, and others).
- In claude-org-ja: `knowledge/curated/`, `knowledge/raw/`.
- Nearest in makiba: the curator writes `<knowledge>/raw` and `curated` for new work (spec sections 2 and 12); the old notes are Japanese and are not copied.
- Mark: **build**, as a review pass before M7-3; Issue #270.
- What the pass is: not a copy of the notes and not a translation of all of them. Each note is either dropped (it is about a layer makiba lacks, or code or a test already holds it) or carried to a named place: a rule in `roles/`, a skill, an incident row with a test, or a curated note. The pass leaves a short record of what was done with each note.
- Reason: what the old notes learned is worth keeping only where it changes what makiba does; copying them would keep the layers makiba dropped.
- Also in this pass: compare claude-org-ja's `docs/contracts/knowledge-curation-contract.md` with `roles/curator/rules.md` and `skills/curate/SKILL.md`. These rules of the contract are missing from makiba's curator rules and skill, and are listed as the rules to carry:
  - a. Edit policy of curated notes: the curator may restructure, merge or delete sections, in or out of a cycle, with one invariant: no silent removal of substantive content.
  - b. A `deferred` status for a candidate the person holds: not terminal, revisited later, not counted toward the batch question. `makiba candidate` takes only approved, rejected and into-<skill>.
  - c. A pattern that returns: new notes merge into an existing pending entry instead of a duplicate, and the contract says what happens when the pattern returns after a terminal status.
  - d. A count for asking the person: the contract batches at 5 pending; the skill says "a batch" with no number.
  - e. No operator-private content in tracked files (workers told in their brief; the secretary routes a scrub). Needed before M7-3, because the repository is public.
  - f. Minor: archived notes never move back out of `raw/archive/`; guidance on naming curated topics.

**N19. Goal-setup dialogue.**
- Does: writes or revises a project's goal ledger by asking the person what they aim at and what would show it unmet, proposes clauses from the README and the open Issues, and validates the file.
- In claude-org-ja: `.claude/skills/goal-setup/`, `tools/work_discovery_goals.py validate`.
- Nearest in makiba: the ledger format and its validator are ported (`src/work_discovery/goals.rs`, `docs/goals.md`); there is no skill that holds the dialogue.
- Mark: **build**, needed at M7-3, which moves the ledgers and registers new projects; Issue #271.
- Reason: without it the person edits ledger files by hand.

**N20. Curation notes of the dispatcher's threshold and archive.**
- Does: the model dispatcher launches the curator at a threshold, monitors it, and archives curated state.
- In claude-org-ja: `.dispatcher/references/curate-monitoring.md`, `tools/check_curate_threshold.py`, `tools/state_db/curator_archive.py`, `.curator/CLAUDE.md`.
- Nearest in makiba: `makibad` launches the curator at the threshold after a close (M5-4, `src/daemon/curator.rs`).
- Mark: **drop** for the archive and the monitoring prose; the threshold and launch are carried.
- Reason: the archive wrote claude-org-ja's `.state` files, which makiba does not have.

### E. State files and records of the old layout

**N21. State snapshots and worker files.**
- Does: writes `.state/org-state.md`, a JSON snapshot and per-worker files from the database, and archives stale worker files.
- In claude-org-ja: `tools/state_db/snapshotter.py`, `tools/sweep_stale_workers.py`, `tools/state_db/migrate_workers.py`, `tools/state_db/importer.py`, `tools/state_migrate.py`.
- Nearest in makiba: `state.db` alone, with versioned SQL migrations (`src/db/schema_v*.sql`). plan.md section 7 lists the snapshotter and the worker-file archive as dropped.
- Mark: **drop**.
- Reason: makiba has one record, not a record and its copies.

**N22. Drift check between the database and files.**
- Does: warns when `state.db` runs and the files on disk disagree, with recovery steps.
- In claude-org-ja: `tools/check_state_drift.py`, `tools/state_db/drift_check.py`, `docs/operations/state-drift-recovery.md`.
- Nearest in makiba: rebuild of the view from base labels (M2-5) and recovery of a launch after `makibad` dies (M3-7).
- Mark: **drop**.
- Reason: with no second copy there is no drift; the case that remains (database against terminals) is M2-5.

**N23. The runtime package and its release flow.**
- Does: the Python package `claude-org-runtime` runs the broker and holds shared logic; its releases are paired with expectation syncs in claude-org-ja.
- In claude-org-ja: `tools/update_runtime.py`, `tools/check_runtime_version.py`, `tools/check_runtime_schema_drift.py`, `tools/check_role_configs.py`, `.claude/skills/runtime-release-with-paired-ja-sync/`, `pyproject.toml`.
- Nearest in makiba: no dependency on it (plan.md section 5); the one binary is released by #62.
- Mark: **drop**.
- Reason: makiba depends on neither package, so there is no pair to keep in sync.

**N24. The screenshot recipe for a worker.**
- Does: the recipe a worker uses to photograph a preview server's page with Windows Chrome from WSL.
- In claude-org-ja: `.claude/skills/wsl-windows-chrome-screenshot/`.
- Nearest in makiba: none.
- Mark: **drop**.
- Reason: it was written for rondo's screens, and rondo is frozen; a worker that needs a screenshot can be given the recipe by a project's own skill.

### F. Transport and platform

**N25. The two transports and their switch.**
- Does: runs the organization on tmux (broker) or on renga panes, with an environment switch and a conversation for changing it.
- In claude-org-ja: `tools/transport.py`, `docs/design/transport-switch-ux.md`, `docs/design/renga-decoupling.md`, `docs/design/renga-pane-control-history.md`, `tools/check_renga_compat.py`, `docs/operations/renga-pane-conventions.md`, `tools/peer_notify.py`, `.claude/rules/`.
- Nearest in makiba: one base, herdr-core, behind one module (plan.md section 3.1).
- Mark: **drop**.
- Reason: two transports side by side caused incidents and most of the explaining; plan.md section 7 lists it as not carried.

**N26. Pane-control guards of the model dispatcher.**
- Does: guards that stop a relative `close_pane` closing the secretary, background-tab workers, attribution when a pane exits, and pane-missing checks.
- In claude-org-ja: `docs/design/close-pane-guard.md`, `.hooks/block-relative-close-pane.sh`, `.hooks/block-blind-spawn-keys.sh`, `.dispatcher/references/pane-close.md`, `pane-exited-attribution.md`, `pane-missing-verification.md`, `pane-output-detection.md`, `background-tab-worker.md`.
- Nearest in makiba: no model operates a terminal, and a terminal is ended by `makibad` through the base module (M5-2).
- Mark: **drop**.
- Reason: its whole subject is a model driving panes.

**N27. The Windows layer.**
- Does: PowerShell installers and watchers, cp932 handling, WezTerm as the Windows backend.
- In claude-org-ja: `scripts/install.ps1`, `tools/pr-watch.ps1`, the Windows notes in `CLAUDE.md`.
- Nearest in makiba: none; plan.md section 7 lists "the Windows layers" as not carried, and the base is run on Linux and macOS.
- Mark: **later**, only if herdr-core supports Windows and a person needs it; Issue #278.
- Reason: a platform is a measurement of the base first, which is not made.

**N28. Docker distribution.**
- Does: an image and compose files that run the organization in a container, with a sandbox canary, an opt-in for Docker inside workers, multi-architecture builds and a published image for a small server.
- In claude-org-ja: `docker/` (`Dockerfile`, `compose.yaml`, `compose.docker-optin.yaml`, `entrypoint.sh`, `install-herdr.sh`, `managed-settings.json`, `sandbox-canary.sh`, `README.md`), `docs/design/org-docker-distribution.md`.
- Nearest in makiba: the single binary of #62 for installation. plan.md section 4 lists "Docker distribution and the server installation" as out, "each a later plan".
- Mark: **later**, after #62 and M7-3; Issue #279.
- Reason: wanted for a server install, but it depends on the release being a binary first.

**N29. Packaging as an app, and herdr-app mods.**
- Does: design notes on making the organization an app, and a measurement of developing mods for the base's app.
- In claude-org-ja: `docs/design/org-app-packaging-options.md`, `docs/design/herdr-app-mod-measurement.md`, `docs/design/herdr-core-measurement.md`.
- Nearest in makiba: the probes (`docs/design/probes.md`) and the runbook `docs/runbooks/herdr-app.md`.
- Mark: **drop** as features (they were notes, with no code); keep as history.
- Reason: no implementation exists to carry, and the screen (N1) is the product direction.

### G. Process files that are not features

**N30. Prose and layout of the old dispatcher.**
- Does: the model dispatcher's instructions, templates and a `.in` generator for skill prose.
- In claude-org-ja: `.dispatcher/CLAUDE.md`, `.dispatcher/references/*.md(.in)`, `tools/skill_src/`, `tools/gen_skill_prose.py`, `docs/design/prompt-modernization*`, `.claude/commands/`, `.claude/agents/`.
- Nearest in makiba: the dispatcher as code (`src/daemon/`); skills written once in English, with no templates.
- Mark: **drop**.
- Reason: plan.md section 7 lists `.dispatcher/` and the skill templates as not carried; `rationale.md` is carried as a document.

## Counts

| Mark | Entries |
| --- | --- |
| build | N13, N18, N19 (3) |
| drop | N2, N6, N8, N9, N10, N11, N12, N14, N15, N20, N21, N22, N23, N24, N25, N26, N29, N30 (18) |
| later | N1, N3, N4, N5, N7, N16, N17, N27, N28 (9) |


## Appendix: carried, and where

Each row is a feature of claude-org-ja that makiba has. It is here so that a reader can check nothing was skipped.

| claude-org-ja feature | In makiba |
| --- | --- |
| Secretary as the one contact; workers never address the person | plan.md section 1; `roles/secretary/rules.md`; the door (spec section 3) |
| Delegating work to a worker (`org-delegate`) | `skills/delegate`; `makiba dispatch` (M3) |
| Escalation of a worker's judgment to the person | `skills/escalation`; `makiba pending`, `answer` (M6-4, M6-8) |
| Pull request, CI watch, review loop, final close (`org-pull-request`) | `skills/pull-request`; PR watches owned by `makibad` (M5-3) |
| Update a branch (`org-update-branch`) | `skills/update-branch`; `makiba update` |
| Issue creation with a goal clause | `skills/issue` |
| Retrospective (`org-retro`) | `skills/retro`; the retrospective moves to the secretary (plan.md section 5) |
| Knowledge curation (`org-curate`), the curator | `skills/curate`, `roles/curator`; `makibad` launches it at the threshold (M5-4); `makiba note`, `candidate` |
| Conveyor, the approved range, stop lines | `skills/conveyor`; belt.md; Issues #257 and #248 |
| Work discovery (triage of open Issues against the goal ledger) | `src/work_discovery/`; triage scan forwarded to the secretary (M5-5) |
| Goal ledger format and validator | `docs/goals.md`; `src/work_discovery/goals.rs` (the dialogue is N19) |
| Pending decisions register | the `pending` table; `makiba pending` (M6-4) |
| Notify gate (what the dispatcher tells the secretary) | `src/daemon/notify_gate.rs` (M2-6) |
| Dispatcher's monitoring, anomaly rules, stall detection | `src/daemon/monitor.rs`, `anomalies.rs` (M2-3) |
| Relay and the delivery ledger (`event_deliveries`) | `messages` and `event_deliveries`; the channel and the inbox (M2-4, M2-7, M2-8) |
| Spawn verification and startup dialogs | M3-5, M3-6, M3-7 (`src/daemon/dialogs.rs`, `verify.rs`) |
| Worker worktree, per-task settings, boundary | M3-2, M4-2, M4-3; `roles/*/settings.json` |
| Git guard hooks (push, dangerous git, no-verify, workers-delete, boundary, foreground subagent) | `makiba hook <name>` (M4-1, M4-3); lines.md |
| Secret scanner in git hooks | `hook pre-commit` and its sibling hooks (L-K12) |
| Acceptance gate and merge gate; merge approval mode | `makiba gate`; `merge_approval_mode` (`config`); `src/daemon/gate.rs`; Issues #47, #263 |
| A worker's second-model review, capped outside the worker | the door's verb `review` and a count in `state.db` (Issue #135, landed); line L-K10 |
| Close a run (`close_run`) | `makiba close` (M5-1, M5-2) |
| Suspend and resume (`org-suspend`, `org-resume`, `org-down`) | `makiba down` and `up` (M5-6); `src/daemon/resume.rs` |
| Secretary handover and resume | `makiba renew` (M6-5); the handover is stored and the fresh secretary launched by `makibad` |
| Registry of projects and org settings | `makiba project`, `makiba config` |
| Language settings of the conversation and of artifacts | `roles/locale/`; `conversation_language`, `artifact_language` (M6-7) |
| Journal of events | the `events` table and `makiba events` |
| PR-watch pane (`pr-watch-pane`, `pr_watch.py`) | owned by `makibad` (M5-3); the pane is gone, the watch is a record |
| Records schema and writer | `src/db/schema*.sql`, `src/db/writer.rs` (M0-5) |
| Delegation lifecycle and state semantics contracts | `docs/contracts/` (translated) |
| Past incidents as tests | `docs/design/incidents.md` (M0-6) |
| The lines that must hold | `docs/design/lines.md` |
| Capacity and worker limit | `max_concurrent_workers` (`config`) |
| English-only check | `tools/check_language.py` (M0-7) |
