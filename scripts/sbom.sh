#!/bin/bash
# A CycloneDX 1.5 software bill of materials for the release binary, built
# from `cargo metadata` (exact versions, sources and checksums of every
# crate compiled in).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo metadata --locked --format-version 1 --filter-platform aarch64-apple-darwin | python3 -c '
import json, sys, uuid, datetime
m = json.load(sys.stdin)
root = m["resolve"]["root"]
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
seen, stack = set(), [root]
while stack:
    i = stack.pop()
    if i in seen: continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
            stack.append(d["pkg"])
pk = {p["id"]: p for p in m["packages"]}
comps = []
for i in sorted(seen - {root}):
    p = pk[i]
    comps.append({"type": "library", "name": p["name"], "version": p["version"],
                  "purl": f"pkg:cargo/{p[\"name\"]}@{p[\"version\"]}",
                  "licenses": [{"expression": p.get("license") or "NOASSERTION"}]})
r = pk[root]
print(json.dumps({"bomFormat": "CycloneDX", "specVersion": "1.5", "serialNumber": f"urn:uuid:{uuid.uuid4()}", "version": 1,
  "metadata": {"timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "component": {"type": "application", "name": "ZClients", "version": r["version"]}},
  "components": comps}, indent=1))
'
