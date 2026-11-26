# Role settings

Every role's `settings.json` denies the base's command and folders, the person key file and any run outside the sandbox, because the base has no permission boundary of its own and the person key is all that separates the person's verbs from an agent's (spec sections 1 and 3).

These files are static, so they name the person key and the base's configuration folder (`~/.config/makiba/base`, spec section 2) at their default places and leave out the base's socket folder; a key moved by `MAKIBA_PERSON_KEY` or `XDG_CONFIG_HOME`, a configuration folder moved by `XDG_CONFIG_HOME`, and the socket folder, are denied by the per-task settings written at launch (Issues 32 and 133).

Each role also has a rules file, `rules.md`, carried from paddock's templates with the mailbox replaced by the door's tools and the two wrapper sentences of spec section 8 added. Its settings name the hooks of the binary (`makiba hook <name>`, spec section 14), ported from claude-org-ja's hooks with the cases of their tests (M4-3):

| Hook | Tool | Roles | Refuses |
| --- | --- | --- | --- |
| `deny-base` | Bash | all | the base's command (M4-1) |
| `deny-push` | Bash | worker, curator | pushing, and every `gh` call that writes to GitHub |
| `deny-dangerous-git` | Bash | all | force pushes, hard resets, stashes that change something, adding everything, and the like |
| `deny-no-verify` | Bash | secretary, curator | skipping the repository's git hooks (git commit's flag read in the text) |
| `deny-no-verify-code-run` | Bash | worker | skipping the repository's git hooks, except git commit's flag, which git's own `prepare-commit-msg` refuses in a code run (Issue 300) |
| `deny-workers-delete` | Bash | all | a recursive `rm` naming the folder of worktrees or of document folders |
| `worker-boundary` | Edit, Write | worker, curator | a write outside the agent's folder, or to its own settings or `.mcp.json` |
| `deny-foreground-subagent` | Agent, Task | all | a subagent in the foreground |
| `premise-check` | UserPromptSubmit | secretary | nothing: it prints one reminder line when the person's message states a number, a setting or a behaviour (Issue 269) |

Every hook refuses a payload it cannot read. What holds each line the organization keeps is in [docs/design/lines.md](../docs/design/lines.md). The binary carries these files. A worker's launch passes `$MAKIBA_SHARE_DIR/roles/worker/rules.md`, written there by `makibad`, as its rules, and its per-task settings file is this role's `settings.json` with the task's denies and write surface added (spec section 9, Issue 134).
