# makiba goals

Clauses higher up win. Each Issue names the clause it serves.

## G1 One contact: the person deals with the secretary only
The reason makiba exists. Everything below serves it.
- unmet if: a worker's stop (a question, a permission prompt, an error) can reach the person without passing through the secretary
- unmet if: a notification to the person can come from a terminal other than the secretary's
- unmet if: renewing the secretary loses an open pending entry or a running task
- unmet if: a skill or a rules file tells the person to open a worker's terminal

## G2 Past incidents do not come back
Every row of docs/design/incidents.md is guarded by a test.
- unmet if: a row of the incident table names no test, or names a test file that does not exist
- unmet if: a test named in the incident table fails

## G3 No model touches a terminal or the ledger directly
- unmet if: a skill or a rules file names a terminal tool or runs the base's command (hc)
- unmet if: a worker, the curator or the secretary can run the base's command or reach its socket
- unmet if: a request with no key is accepted at the door, or the person key can be read from inside an agent's sandbox
- unmet if: any role other than the dispatcher can write state.db

## G4 Dispatch and close record what was observed
- unmet if: a path exists that records a launch as verified without an observed first turn
- unmet if: stopping the dispatcher part-way leaves a second terminal or a stranded run
- unmet if: a run can be closed before its done report without an explicit abandon

## G5 The watch does not stop, and if it stops it shows
- unmet if: nothing shows that the dispatcher has stopped
- unmet if: a terminal event is missed after the base restarts
- unmet if: a message to the secretary can be lost
- unmet if: a message is typed twice without the dispatcher having died in between

## G6 The base is a part that can be replaced, and an update of it does not break makiba
- unmet if: the base's version is not pinned
- unmet if: code outside makiba/base/ names the base
- unmet if: the base supplies rules to agents or sends notices to the person
- unmet if: no test checks the operations and fields makiba uses against the base's own definition of them

## G7 It runs its own development
- unmet if: makiba's development still runs on claude-org-ja
- unmet if: an Issue of milestones M0 to M6 is open
