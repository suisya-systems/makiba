# The pass over claude-org-ja's notes

Issue #270, entry N18 of [not-carried.md](not-carried.md). Done on 2026-11-22, before M7-3.

claude-org-ja kept what it learned as curated notes, one file per topic, each merged from raw notes. This pass read every curated note whole and decided, for each, drop or carry. Nothing was copied or translated: a note changes nothing unless a worker, the curator, the secretary or a rule reads it, so a lesson was carried only to a place that is read.

Notes read: 43. Dropped: 24. Carried: 19.

- A carried note is carried in part: only the lessons that would change what an agent of makiba does, and that makiba did not hold, went anywhere. 50 lessons were carried from the 19 notes; the rest of each note was dropped for the reason its row gives.
- The 587 archived raw notes were not judged one by one: each had been merged into a curated note, and was judged through it. No raw note was waiting unmerged.
- The queue of skill candidates (23 entries: 17 rejected, 4 held, 1 approved, 1 pending) was dropped whole: the approved one is a recipe for another product's screens, and the pending one is the procedure for a new version of Claude Code, which `docs/upgrading-claude-code.md` holds.
- 36 of the 43 notes are untracked, operator-private files of claude-org-ja. This record therefore holds counts, topics and places only.

## Where a lesson could go

| Place | What went there |
| --- | --- |
| `roles/worker/rules.md` | Four sentences: measure a review finding before obeying it and fix every instance of its kind in one commit; a hook refuses a whole call by its text, and text that only names a guarded command goes by file; a subagent's brief says where it may write. |
| `skills/delegate/SKILL.md` | Two items of the brief: an Issue's text word for word; what a task with a declared acceptance test is told. |
| `skills/issue/SKILL.md` | No details of a dependency's unfixed defect in a public Issue. |
| `docs/design/incidents.md` | Row I-12, with its test in `tests/notes_pass.rs`. |
| The knowledge folder | 12 raw notes, written in English from the carried lessons and saved by the secretary with `note` under task `r2-270`, so that the curator merges them as it does any note. They are named below as `raw/<file>`. A thirteenth, `raw/2026-11-22-notes-pass-grounds-corrections.md`, corrects the grounds of three of them, which said more than the sources did; a saved note is never edited. |

No lesson became a new skill.

## Every note

`layer` means a layer makiba does not have: renga, tmux, the broker, the model dispatcher, claude-org-ja's Python tools, its state files, its English mirror. `other stack` means the tools of another product (TypeScript, npm, a browser, a web page). `held` means makiba's code, a test, a rule or a document already holds it. `generic` means what any engineer does without a note.

| Note | Verdict | Why dropped, or what was carried | Where it went |
| --- | --- | --- | --- |
| `agentic-loop` | drop | layer and other stack; the headless-session flags are held by the review code | |
| `api-protocol-design` | carry | how a new status field is documented without breaking the test of the read surface; the rest layer, other stack and generic | `raw/2026-11-22-makiba-tests-in-a-worker-sandbox.md` |
| `architecture-boundary-checks` | drop | other stack; a deny list over a command one does not own is held by the push hook; review rounds are held by the worker's rules | |
| `async-loop-internals` | drop | other stack (a Python event loop); the stale-base point is held by the review's merge base | |
| `broker-transport` | carry | how Claude Code expands variables in the headers of its MCP configuration; the rest layer, and delivery is held by the ledger | `raw/2026-11-22-claude-code-cli-arguments.md` |
| `browser-automation` | carry | two facts about the shell an agent's commands run in; the rest other stack | `raw/2026-11-22-agent-shell-is-zsh.md` |
| `claude-ai-connectors` | drop | other stack | |
| `claude-cli-headless-reader` | carry | which model answers a headless session, and how its failures read; the flags are held by the review code | `raw/2026-11-22-the-two-readers-codex-and-claude-print.md`, `raw/2026-11-22-notes-pass-grounds-corrections.md` |
| `codex` | carry | why codex can answer clean without reading, and how its model and version refusals read; the read proof, the round count and the fallback are held by the review code and its tests | `raw/2026-11-22-the-two-readers-codex-and-claude-print.md`, `raw/2026-11-22-notes-pass-grounds-corrections.md` |
| `concurrency-locking` | carry | a joined transaction that swallows an error; a lock on a tool's exit code that broke in CI only; the rest layer, held (delivery, launch stages) and generic | `raw/2026-11-22-sqlite-triggers-constraints-and-migrations.md`, `raw/2026-11-22-github-and-ci-behaviour.md` |
| `contract-ratification` | drop | other products' ledgers and contracts; generic | |
| `cross-language-porting` | carry | three habits for the review and for subagents; facts about signals and `/proc`; an interactive alias in the agent's shell; the rest other stack (the port is finished) and generic | `roles/worker/rules.md`, `raw/2026-11-22-signals-pids-and-proc.md`, `raw/2026-11-22-agent-shell-is-zsh.md`, `raw/2026-11-22-notes-pass-grounds-corrections.md` |
| `cross-platform-cli` | carry | three zsh traps; Claude Code's options that swallow a following word; the rest other stack | `raw/2026-11-22-agent-shell-is-zsh.md`, `raw/2026-11-22-claude-code-cli-arguments.md` |
| `debugging-investigation` | carry | a command kept in a variable that never ran; proving "not the source" with a binary's hash; the rest other stack and generic | `raw/2026-11-22-agent-shell-is-zsh.md`, `raw/2026-11-22-cargo-and-rust-builds-in-a-worker.md` |
| `delegation` | carry | what a brief must hold; no unfixed defect of a dependency in a public Issue; tests a worker's sandbox cannot run; a new file left untracked; the rest layer and held (the worker's rules, the retro skill) | `skills/delegate/SKILL.md`, `skills/issue/SKILL.md`, `raw/2026-11-22-makiba-tests-in-a-worker-sandbox.md`, `raw/2026-11-22-git-in-a-worker.md` |
| `dispatcher-monitoring` | drop | layer (the model dispatcher's watch cycle); the two-route gap is incident I-5 | |
| `en-mirror-migration` | drop | layer | |
| `environment-bootstrap` | drop | other stack; the build folder and the commit check are held by the launch and the gate code | |
| `factory-lap` | drop | other stack | |
| `frontend-htmx` | drop | other stack | |
| `generated-artifacts` | drop | layer (generated skill text) and other stack | |
| `git-workflow` | carry | what a write-denied git `config` does to a new branch; a merge git calls clean and is wrong; two workers taking the next number of a list; what CI builds for a pull request; the rest layer and held (the worker's rules, the hooks, the gate). One lesson was not carried here because it changes git guidance: it is proposed to the secretary as a follow-up | `raw/2026-11-22-git-in-a-worker.md`, `raw/2026-11-22-github-and-ci-behaviour.md` |
| `github-cli` | carry | the scope a push of a workflow file needs; a pull request left without a run by an outage; the lag of the search index; the rest held (the gate reads only pull-request runs) and generic | `raw/2026-11-22-github-and-ci-behaviour.md` |
| `herdr` | carry | two facts about the base's command and the app; the rest is an old adapter makiba does not have, and the trust dialog is incident I-9 | `raw/2026-11-22-herdr-core-and-the-systemd-user-manager.md`, `raw/2026-11-22-notes-pass-grounds-corrections.md` |
| `llm-eval-design` | drop | other products' experiments; generic | |
| `multi-locale-docs` | drop | layer | |
| `npm-dependency-resolution` | drop | other stack | |
| `oss-evaluation` | drop | other stack; generic | |
| `permissions-sandbox` | carry | how Claude Code applies a settings file; the sandbox's mounts and its network list; cargo with a read-only home; a hook reads text; the socket tests; the rest other stack, layer and held (the hooks' cases, the probes, the per-task settings) | `raw/2026-11-22-claude-code-sandbox-behaviour.md`, `raw/2026-11-22-makiba-tests-in-a-worker-sandbox.md`, `raw/2026-11-22-cargo-and-rust-builds-in-a-worker.md`, `roles/worker/rules.md`, `docs/design/incidents.md` |
| `product-scope-decisions` | drop | another product's decisions | |
| `refactoring-extract-module` | drop | other stack; generic | |
| `release-process` | drop | other stack (a change log makiba does not keep) | |
| `reporting-artifacts` | carry | command blocks written into a procedure from memory; the rest other stack and layer | `raw/2026-11-22-runbook-command-blocks.md` |
| `runtime-as-source-of-truth` | carry | a session started inside a sandbox can run with its own sandbox off; the rest layer, other stack and held (the base's pin is checked against the real base) | `raw/2026-11-22-claude-code-sandbox-behaviour.md` |
| `schema-validation` | drop | layer | |
| `scriptless-ui-design` | drop | other stack | |
| `sqlite-persistence` | carry | triggers, constraints, JSON columns and table rebuilds in SQLite; the rest other stack and held (the write lock, the tests of old and failed migrations) | `raw/2026-11-22-sqlite-triggers-constraints-and-migrations.md` |
| `state-resume-replay` | drop | other stack and layer; "record the intent first" is held by the launch stages and their tests | |
| `systemd-wsl-services` | carry | linger and the user manager's `PATH`; what a sandboxed command cannot learn about the host; the rest another product's setup | `raw/2026-11-22-herdr-core-and-the-systemd-user-manager.md`, `raw/2026-11-22-claude-code-sandbox-behaviour.md` |
| `testing` | carry | five facts about makiba's own tests and the shell they run in; the rest other stack, layer and held (the acceptance checker, the probes) | `raw/2026-11-22-makiba-tests-in-a-worker-sandbox.md`, `raw/2026-11-22-agent-shell-is-zsh.md` |
| `tui-automation` | drop | layer (screen reading in tmux and renga) | |
| `typescript-tooling` | drop | other stack | |
| `wsl-tui` | drop | other stack (makiba draws no screen) | |

## The rules of the curation contract

claude-org-ja's `docs/contracts/knowledge-curation-contract.md` was compared with the curator's rules and the curate skill. What makiba lacked:

| Rule | Where it is now |
| --- | --- |
| a. The curator may restructure a curated note; nothing of substance goes silently | `roles/curator/rules.md` |
| b. `deferred`: the person holds a candidate; not a decision | the verb `candidate` (`src/daemon/door.rs`, spec section 12), `skills/curate/SKILL.md` |
| c. A pattern that returns | `roles/curator/rules.md`, `skills/curate/SKILL.md`, and the verb, which writes the pattern's one open entry |
| d. The count for asking the person: 5 pending | `skills/curate/SKILL.md` |
| e. Nothing operator-private in a tracked file | `roles/worker/rules.md`, `roles/curator/rules.md`, `roles/secretary/rules.md`, `skills/issue/SKILL.md` |

In makiba the knowledge folder is outside every repository and is not tracked, so rule e is about what a worker commits and what the curator writes for a later skill, not about the folder.

Differences from the contract, on purpose:

- A pattern returns only on notes saved after its most recent decision. makiba's curator reads every note in every cycle; without that bound a decided pattern would be proposed again each time.
- The curator adds new notes to a `deferred` entry as it does to a `pending` one, so that the entry is current when the person comes back to it.
- Rule f (minor): "archived notes never move back" is a clause of the curator's rules; the guidance on naming curated topics was not carried.
