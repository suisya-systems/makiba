# Role settings

Every role's `settings.json` denies the base's command and folders, the person key file and any run outside the sandbox, because the base has no permission boundary of its own and the person key is all that separates the person's verbs from an agent's (spec sections 1 and 3).

These files are static, so they name the person key at its default place and leave out the base's socket folder; a key moved by `MAKIBA_PERSON_KEY` or `XDG_CONFIG_HOME`, and the socket folder, are denied by the per-task settings written at launch (Issue 32).
