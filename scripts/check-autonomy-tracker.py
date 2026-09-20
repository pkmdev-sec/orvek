#!/usr/bin/env python3
"""Check the autonomy research tracker's local evidence and dependency ledger.

This is a research snapshot check, not a runtime or external-source verifier.
A changed source hash requires reviewing the associated finding before refreshing it.
"""

import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "docs/design/autonomy-tracker.sources.json"


def check(manifest):
    failures = []
    text = (ROOT / manifest["tracker"]).read_text()
    sources = manifest["external_sources"]
    evidence = manifest["code_evidence"]
    items = manifest["work_items"]
    for label, records in (("source", sources), ("evidence", evidence), ("work item", items)):
        ids = [record["id"] for record in records]
        if len(ids) != len(set(ids)):
            failures.append(f"Duplicate {label} ID")
        for identity in ids:
            if not re.search(rf"\b{re.escape(identity)}\b", text):
                failures.append(f"Tracker omits {label} {identity}")
    for record in sources:
        if record["url"] not in text:
            failures.append(f"Tracker omits source URL {record['id']}")
        if record.get("article_url") and record["article_url"] not in text:
            failures.append(f"Tracker omits companion article {record['id']}")
    for record in evidence:
        path = ROOT / record["path"]
        if not path.is_file():
            failures.append(f"{record['id']}: missing {record['path']}")
            continue
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != record["sha256"]:
            failures.append(f"{record['id']}: source changed; review {record['path']}")
        if record["anchor"] not in data.decode():
            failures.append(f"{record['id']}: missing anchor {record['anchor']!r}")
    by_id = {item["id"]: item for item in items}
    source_ids = {record["id"] for record in sources}
    evidence_ids = {record["id"] for record in evidence}
    if sorted(item["rank"] for item in items) != list(range(1, len(items) + 1)):
        failures.append("Ranks must be unique and consecutive")
    for item in items:
        identity = item["id"]
        if not re.search(rf"^### {identity}:", text, re.MULTILINE):
            failures.append(f"{identity}: missing work card")
        dependencies = set(item["depends_on"])
        for phase in item.get("phase_dependencies", {}).values():
            dependencies.update(phase)
        for dependency in dependencies:
            if dependency not in by_id or by_id[dependency]["rank"] >= item["rank"]:
                failures.append(f"{identity}: unknown, cyclic, or later dependency {dependency}")
        if not item["sources"] or not set(item["sources"]) <= source_ids:
            failures.append(f"{identity}: missing or unknown source references")
        if not item["code_evidence"] or not set(item["code_evidence"]) <= evidence_ids:
            failures.append(f"{identity}: missing or unknown code references")
        if item["status"] == "done" and not item["results"]:
            failures.append(f"{identity}: done without recorded results")
    return failures


def main():
    manifest = json.loads(MANIFEST.read_text())
    failures = check(manifest)
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(
        f"Checked {len(manifest['external_sources'])} supplied sources, "
        f"{len(manifest['code_evidence'])} local evidence anchors, and "
        f"{len(manifest['work_items'])} work items. "
        "No external freshness, feature acceptance, or competitor benchmark claim."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
