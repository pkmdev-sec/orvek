#!/usr/bin/env python3
"""Check explicit guide-fence classifications and render real host examples."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
EXAMPLES = Path("examples/host-docs")
DOCUMENT = Path("docs/executable-examples.md")
FENCES = re.compile(r"^```([^\n]*)\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
SCENARIOS = {
    "native": "Native completion without certification",
    "sandbox": "Verified sandbox completion",
    "reconnect": "Lost acknowledgement and journal reconnect",
    "completion_hook": "Local terminal hook delivery",
}


def inventory_errors(root, rows):
    blocks = {}
    guides = [root / "README.md", *sorted((root / "docs").glob("*.md"))]
    for guide in guides:
        if guide == root / DOCUMENT:
            continue
        for number, (language, body) in enumerate(FENCES.findall(guide.read_text()), 1):
            blocks[(str(guide.relative_to(root)), number)] = hashlib.sha256(
                (language + "\n" + body).encode()).hexdigest()
    errors = []
    seen = set()
    for row in rows:
        key = row["guide"], row["block"]
        if key in seen:
            errors.append(f"duplicate classification: {key}")
        seen.add(key)
        if key not in blocks:
            errors.append(f"removed or unknown guide fence: {key}")
        elif blocks[key] != row["sha256"]:
            errors.append(f"changed guide fence; review classification and hash: {key}")
        # Runnable means a setup/assert/teardown scenario below, never arbitrary guide text.
        if row["kind"] not in {"illustrative", "external-service"}:
            errors.append(f"invalid guide classification: {key}")
        if not row["reason"].strip():
            errors.append(f"classification needs a reason: {key}")
    errors.extend(f"unclassified guide fence: {key}" for key in sorted(blocks.keys() - seen))
    return errors


def render(root, rows):
    text = """# Executable host examples

These examples run the real CLI and same-user operator IPC against a loopback fake
Responses provider. They use Python 3.11+ and its standard library. No live provider
or notification service is used. Assertions check bytes, state and receipts, not
assistant prose. This is deterministic wiring coverage, not a model-quality test.

## Run

Build `orvek` with `cargo build --locked --package orvek --bin orvek`.
Pass the built binary path as the only argument to each scenario below.
Do not use Python's `-O` flag: it disables assertions.

Native, reconnect and hook scenarios require Unix, but not Docker. The sandbox
scenario also requires local Docker, a locally available `debian:bookworm-slim`
image, and `ORVEK_EXECUTOR_HELPER` pointing to a matching Linux helper. See
[workspace execution](workspace-execution.md) for helper setup. Missing prerequisites
fail; no case silently skips. CI runs all four in the Docker security job.

The shared [fixture](../examples/host-docs/fixture.py) starts a separate foreground
host, local provider, configuration, HOME and workspace for each run. It stops the
host/provider and removes its temporary files on success or failure. Provider
credentials, proxy settings and user configuration are not inherited. Docker
connection settings are retained only for access to the operator's local engine.
Never point an example at your own workspace. Native execution is not a sandbox;
these examples control the provider and use only the temporary workspace.

## Coverage limits

- **Memory scan/read and skill discovery: pending T01.** Configuration and JSON
  shapes in the guides are illustrative, not proof that the host exposes them.
  Add examples that inspect actual tool outputs and persistence after integration.
- **Sanitized trace links: pending T04.** Journal assertions below are not portable
  trace bundles. No trace exports or offline replay claims are made here.
- Reconnect drops an IPC acknowledgement and reconnects a journal watch. It does
  not kill or restart the host.
- The hook writes only a local payload, then exits with code 9. A failed delivery
  must not change task truth. This is not proof of external notification delivery
  or exactly-once effects after a host crash.
- Sandbox checks reuse the arithmetic bug and trusted baseline/candidate probes
  from `crates/harness/tests/controller_execution.rs`, through public IPC rather
  than a second verification implementation. Delivery does not overwrite source.

T11 remains partial until the pending examples and trace links are verified.

## Runnable scenarios

Generated from the complete executable files. Edit the source files, then run
`python3 scripts/check-doc-examples.py`. CI uses `--check` and runs the scenarios.
"""
    for name, title in SCENARIOS.items():
        source = EXAMPLES / (name + ".py")
        text += f"\n### {title}\n\nClassification: **runnable**. [Source](../{source}).\n\n"
        text += f"Run `python3 {source} /absolute/path/to/orvek`.\n\n```python\n"
        text += (root / source).read_text().rstrip() + "\n```\n"
    text += """
## Guide inventory

Scope: every fenced block in `README.md` and top-level `docs/*.md`, excluding this
generated page. Design records, the derived codebase graph, and deployment-specific
example READMEs are outside this first inventory. Nothing outside it is claimed as
executed. Block numbers follow source order. Changed or new blocks fail the check
until their classification is reviewed in `examples/host-docs/inventory.json`.
The existing `scripts/check-docs.py` still checks links and syntax separately.

| Guide block | Classification | Reason |
| --- | --- | --- |
"""
    for row in rows:
        text += f"| [{row['guide']}](../{row['guide']}) #{row['block']} | {row['kind']} | {row['reason']} |\n"
    return text


def check(root, rows):
    errors = inventory_errors(root, rows)
    expected = render(root, rows)
    destination = root / DOCUMENT
    if not destination.exists() or destination.read_text() != expected:
        errors.append("stale generated examples: run python3 scripts/check-doc-examples.py")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="reject drift without writing files")
    args = parser.parse_args()
    rows = json.loads((ROOT / EXAMPLES / "inventory.json").read_text())
    errors = check(ROOT, rows) if args.check else inventory_errors(ROOT, rows)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    if not args.check:
        (ROOT / DOCUMENT).write_text(render(ROOT, rows))
    print(f"Checked {len(rows)} guide fences and {len(SCENARIOS)} executable snippets (not executed).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
