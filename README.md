# makiba

makiba is an organization of Claude Code agents in which the person talks to one secretary and nothing else.
The person asks the secretary, is asked by the secretary and gets results from the secretary; never from a worker.
makiba is a single binary written in Rust, and its dispatcher is code; the secretary and the workers are interactive Claude Code sessions in terminals.
makiba is under active development and is not usable yet; M0 and M1 are done and M2, the dispatcher, is in progress.
The terminals run on herdr-core, behind one module so that it can be replaced.
The plan is in [docs/design/plan.md](docs/design/plan.md); how a pull request proves it is test-first is in [docs/acceptance-check.md](docs/acceptance-check.md).
