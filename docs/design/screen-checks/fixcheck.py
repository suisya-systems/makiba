"""Checks that the screen fixture's status.json is what makiba would read back from its
events.json, wherever the events of the page say it: next_work from the newest scan
(status_local::next_work), the open anomalies raised in the page, the places and the tasks used of
each range, the items in the order of their belt_dispatched and belt_skipped, a replacing proposal in
the same project, every escalated stop named by a belt_stopped, increasing event ids, and (Issue 415)
one left-out entry per project of its repository, as triage.rs `shown` writes it.
Run from the repository root: python3 docs/design/screen-checks/fixcheck.py [fixture folder]
Exit 1 on a mismatch."""
import json, sys
d = sys.argv[1] if len(sys.argv) > 1 else 'docs/design/screen-fixture'
s = json.load(open(f"{d}/status.json")); ev = json.load(open(f"{d}/events.json"))["events"]
bad = []
sc = [x for x in ev if x["kind"] == "work_discovery_scanned"][-1]; p = sc["payload"]; nw = s["next_work"]
want = {"scan_id": sc["id"], "scanned_at": sc["occurred_at"], "candidate_count": p["candidate_count"] + p["truncated_count"]}
for k in ["trigger", "status", "error", "input_truncated", "candidates", "candidates_not_recorded", "left_out", "left_out_not_recorded"]:
    want[k] = p.get(k)
for k, v in want.items():
    if nw.get(k) != v: bad.append(f"next_work.{k}: status {nw.get(k)!r:.60} != from the event {v!r:.60}")
if set(want) != set(nw): bad.append(f"next_work keys differ: {sorted(set(nw) ^ set(want))}")
opened, cleared = {}, set()
for x in ev:
    if x["kind"] in ("anomaly_raised", "anomaly_cleared"):
        key = (x["payload"]["task_id"], x["payload"]["rule"])
        if x["kind"] == "anomaly_raised":
            opened[key] = x["occurred_at"]; cleared.discard(key)
        else:
            opened.pop(key, None); cleared.add(key)
have = {(a["task_id"], a["rule"]): a["since"] for a in s["anomalies"]}
for k, v in opened.items():
    if have.get(k) != v: bad.append(f"anomaly {k} raised at {v} in the events, status has {have.get(k)}")
for k in have:
    if k in cleared: bad.append(f"anomaly {k} is cleared in the events, but status still lists it as open")
for r in s["ranges"]:
    pl = {k: 0 for k in r["places"]}
    for i in r["items"]: pl[i["place"]] += 1
    if pl != r["places"]: bad.append(f"range {r['id']}: places {r['places']} != items {pl}")
    used = sum(1 for i in r["items"] if i["task_id"])
    if r["status"] == "active" and used != r["used"]["tasks"]: bad.append(f"range {r['id']}: used.tasks {r['used']['tasks']} != items with a task {used}")
esc = {x["payload"]["pending_id"] for x in ev if x["kind"] == "belt_stopped"}
for pd in s["pending"]:
    if pd["status"] == "escalated" and any(st["id"] == pd["id"] for r in s["ranges"] for st in r["stops"]) and pd["id"] not in esc:
        bad.append(f"pending {pd['id']}: an escalated stop with no belt_stopped in the page")
disp = {x["payload"]["task_id"]: x["id"] for x in ev if x["kind"] == "belt_dispatched"}
for r in s["ranges"]:
    skp = {x["payload"]["issue"]: x["id"] for x in ev if x["kind"] == "belt_skipped" and x["payload"]["range_id"] == r["id"]}
    for sk in r["skipped"]:
        if sk["issue"] not in skp: bad.append(f"range {r['id']}: skipped #{sk['issue']} has no belt_skipped")
    seen = [disp[i["task_id"]] if i["task_id"] in disp else skp[i["issue"]] for i in r["items"] if i["task_id"] in disp or (i["place"] == "skipped" and i["issue"] in skp)]
    if seen != sorted(seen): bad.append(f"range {r['id']}: items not in the order of their belt_dispatched {seen}")
for x in ev:
    if x["kind"] == "range_proposed" and x["payload"].get("replaces") is not None:
        old = [r for r in s["ranges"] if r["id"] == x["payload"]["replaces"]]
        if not old or old[0]["project"] != x["payload"]["project"]: bad.append(f"range {x['payload']['range_id']} replaces a range of another project")
stopped = {x["payload"]["pending_id"]: x["payload"] for x in ev if x["kind"] == "belt_stopped"}
for pd in s["pending"]:
    if pd["status"] == "escalated" and pd["task_id"] is None and pd["id"] not in stopped:
        bad.append(f"pending {pd['id']}: an escalated entry with no task and no belt_stopped to name its range")
# triage.rs `shown`: an entry with a repository is written once per project of that repository
# (case aside), with project null only when none matches. This assumes, as a fixture may, that no
# project was registered or removed between the scan and the status: a real pair of reads may differ
# there without being wrong. The scan's `repos` (the list of repositories it read, triage.rs) against
# the repositories status records is only a coarse guard of that assumption; the check is also
# skipped when the list is cut.
if not isinstance(p.get("repos"), list):
    bad.append(f"work_discovery_scanned.repos is {p.get('repos')!r}, not the list of repositories triage.rs writes")
elif nw["left_out_not_recorded"] or {r.lower() for r in p["repos"]} != {x["repo"].lower() for x in s["projects"] if x["repo"]}:
    print("left-out per project: not checked (the list is cut, or the repositories changed since the scan)")
else:
    owners = {}
    for x in s["projects"]:
        if x["repo"]: owners.setdefault(x["repo"].lower(), []).append(x["slug"])
    groups = {}
    for x in nw["left_out"] or []:
        if x["repo"] is None: continue
        groups.setdefault((x["repo"].lower(), x["issue"], x["kind"], x["reason"]), []).append(x["project"])
    for (repo, issue, kind, _), got in groups.items():
        # A project with no repository recorded may still own it through its clone's origin, which
        # status does not print: such a slug may be among the entries, never required.
        want = owners.get(repo, [None])
        may = set(want) | {x["slug"] for x in s["projects"] if not x["repo"]}
        if not set(want) <= set(got) or not set(got) <= may or len(got) != len(set(got)):
            bad.append(f"left_out {repo}#{issue} {kind}: projects {got} != the projects of the repository {want}")
ids = [x["id"] for x in ev]
if ids != sorted(ids) or len(set(ids)) != len(ids): bad.append("event ids are not increasing")
print("\n".join(bad) or "fixture consistent")
sys.exit(bool(bad))
