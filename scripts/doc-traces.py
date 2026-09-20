#!/usr/bin/env python3
"""Generate or validate local event-only trace bundles from the five host examples."""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
EXAMPLES = Path("examples/host-docs")
TRACES = EXAMPLES / "traces"
SCENARIOS = ("native", "sandbox", "reconnect", "completion_hook", "memory_skills")
POLICY = "controlled-fixture-events-only-v1"

if not __debug__:
    raise RuntimeError("Run trace checks without -O; assertions are the checks")


def sha256(body):
    return hashlib.sha256(body).hexdigest()


def encoded(value):
    # These fixtures contain only integral numbers and ASCII strings. Preserve struct order.
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode()


def source_hashes(root):
    paths = [EXAMPLES / (name + ".py") for name in (*SCENARIOS, "fixture")]
    paths += [Path("scripts/doc-traces.py")]
    return {str(path): sha256((root / path).read_bytes()) for path in paths}


def runtime_hash(root):
    # Hash Rust, Cargo and embedded font inputs without checkout-specific paths.
    paths = sorted({*root.glob("Cargo.*"), *root.glob("rust-toolchain*"),
                    *(p for directory in ("bin", "crates", "vendor", ".cargo")
                      for p in (root / directory).rglob("*")
                      if p.is_file() and p.suffix in (".rs", ".toml", ".lock", ".bin"))})
    return sha256(encoded([(str(p.relative_to(root)), sha256(p.read_bytes())) for p in paths]))


def run_trace(binary, operation, path, *, env=None):
    result = subprocess.run([str(binary), "trace", operation, str(path)], env=env,
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, (operation, result.stdout, result.stderr)
    return json.loads(result.stdout)


def validate_bundle(path):
    envelope = json.loads(path.read_text())
    bundle = envelope["bundle"]
    assert envelope["digest"] == sha256(encoded(bundle)), "bundle hash mismatch"
    assert bundle["version"] == 1 and bundle["after"] == 0, "unsupported trace prefix"
    assert bundle["exact"] is False, "sanitized fixture cannot claim exact replay"
    assert bundle["records"] and bundle["through"] == len(bundle["records"])
    assert bundle["artifacts"], "expected omitted fixture artifacts"
    assert all(payload == {"status": "omitted"} for payload in bundle["artifacts"].values()), \
        "event-only fixtures must omit every artifact payload"
    heads = {}
    for sequence, record in enumerate(bundle["records"], 1):
        assert record["sequence"] == sequence, "journal cursor gap"
        key = record["kind"], record["aggregate"]
        revision, previous = heads.get(key, (0, None))
        assert record["revision"] == revision + 1, "aggregate revision gap"
        body = base64.b64decode(record["event_base64"], validate=True)
        expected = sha256(encoded([*key, record["revision"], previous, sha256(body)]))
        assert record["hash"] == expected, "journal hash mismatch"
        heads[key] = record["revision"], expected
        vet_event(json.loads(body))
    return bundle


def vet_event(value):
    """Guard audited fixture data, including embedded JSON and bytes. Not a general sanitizer."""
    if isinstance(value, dict):
        for key, child in value.items():
            vet_event(key)
            vet_event(child)
    elif isinstance(value, list):
        if value and all(type(child) is int and 0 <= child <= 255 for child in value):
            vet_event(bytes(value).decode("utf-8"))
        else:
            for child in value:
                vet_event(child)
    elif isinstance(value, str):
        if value.startswith(("{", "[")):
            vet_event(json.loads(value))
            return
        assert "docs-fixture-not-a-secret" not in value, "credential-bearing event"
        assert not re.search(r"(?i)(authorization|bearer |api[_-]?key|password|/Users/|/home/)", value), \
            "private event data"
        assert "http://" not in value and "https://" not in value, "endpoint in event"
        if "/" in value:
            assert ".." not in Path(value).parts, "event path escapes fixture"
            assert value in {"./add 2 2", "#!/bin/sh\nprintf '3\\n'\n",
                             "#!/bin/sh\nprintf '%s\\n' \"$(($1 + $2))\"\n"} or re.fullmatch(
                r"/(?:private/)?tmp/ov-doc-[a-zA-Z0-9_-]+/(?:workspace(?:/[a-zA-Z0-9_.-]+)*|skills/check-note/SKILL.md)",
                value), "unreviewed event path"


def assert_replay(binary, path, scenario, *, env=None):
    report = run_trace(binary, "replay", path, env=env)
    assert report["exact"] is False and report["unresolved"], report
    outcome = "complete" if scenario == "sandbox" else "finished_unverified"
    assert report["tasks"] and all(task["outcome"] == outcome for task in report["tasks"].values())
    for task in report["tasks"].values():
        assert bool(task["certificates"]) == (scenario == "sandbox"), "certificate/outcome mismatch"
    review = run_trace(binary, "review", path, env=env)
    assert review, "empty review"
    return {"outcome": outcome, "sessions": len(report["sessions"]), "tasks": len(report["tasks"])}


def assert_false_exact_rejected(binary, path, *, env=None):
    envelope = json.loads(path.read_text())
    envelope["bundle"]["exact"] = True
    envelope["digest"] = sha256(encoded(envelope["bundle"]))
    with tempfile.TemporaryDirectory(prefix="ov-doc-forged-", dir="/tmp") as directory:
        forged = Path(directory) / "false-exact.json"
        forged.write_bytes(encoded(envelope))
        result = subprocess.run([str(binary), "trace", "replay", str(forged)], env=env,
                                capture_output=True, text=True, timeout=120)
        assert result.returncode != 0 and "manifest claims exact replay" in result.stderr, result


def check(root=ROOT, directory=None, binary=None):
    directory = directory or root / TRACES
    errors = []
    try:
        index = json.loads((directory / "index.json").read_text())
        assert index["version"] == 1 and index["policy"] == POLICY, "unknown fixture policy"
        assert index["sources"] == source_hashes(root), "stale trace fixture sources; regenerate"
        assert index["runtime_sha256"] == runtime_hash(root), "stale trace runtime sources; regenerate"
        assert re.fullmatch(r"[0-9a-f]{64}", index["binary_sha256"]), "missing binary identity"
        assert re.fullmatch(r"[0-9a-f]{64}", index["sandbox"]["helper_sha256"]), "missing helper identity"
        assert index["sandbox"]["image"] == "debian:bookworm-slim"
        assert re.fullmatch(r"sha256:[0-9a-f]{64}", index["sandbox"]["image_id"]), "missing image identity"
        assert set(index["scenarios"]) == set(SCENARIOS), "missing/unknown trace scenario"
        assert {p.name for p in directory.iterdir()} == {"index.json", *(name + ".trace.json" for name in SCENARIOS)}, \
            "missing/extra trace fixture files"
        for name, entry in index["scenarios"].items():
            assert re.fullmatch(r"[0-9a-f]{64}", entry["original_digest"]), "missing original export identity"
            assert type(entry["original_exact"]) is bool, "missing original exactness"
            path = directory / (name + ".trace.json")
            assert sha256(path.read_bytes()) == entry["sha256"], f"changed trace: {name}"
            bundle = validate_bundle(path)
            assert len(bundle["records"]) == entry["records"] and path.stat().st_size == entry["bytes"]
            assert sha256(encoded(bundle["records"])) == entry["records_sha256"], "rewritten journal"
            assert bundle["exporter_revision"] == index["exporter_revision"], "mixed exporting binaries"
            if binary:
                assert assert_replay(binary, path, name) == entry["replay"]
                assert_false_exact_rejected(binary, path)
    except (AssertionError, KeyError, ValueError, OSError) as error:
        errors.append(f"trace fixtures: {error}")
    return errors


def generate(binary, directory):
    binary = binary.resolve(strict=True)
    directory.mkdir(parents=True, exist_ok=True)
    # Each file is replaced only after its complete scenario, export, vetting and replay pass.
    sys.path.insert(0, str(ROOT / EXAMPLES))
    index = {"version": 1, "policy": POLICY, "sources": source_hashes(ROOT),
             "runtime_sha256": runtime_hash(ROOT), "binary_sha256": sha256(binary.read_bytes()),
             "scenarios": {}}
    helper = Path(os.environ["ORVEK_EXECUTOR_HELPER"]).resolve(strict=True)
    image = subprocess.run(["docker", "image", "inspect", "debian:bookworm-slim", "--format", "{{.Id}}"],
                           capture_output=True, text=True, check=True, timeout=30)
    index["sandbox"] = {"helper_sha256": sha256(helper.read_bytes()), "image": "debian:bookworm-slim",
                        "image_id": image.stdout.strip()}
    for name in SCENARIOS:
        spec = importlib.util.spec_from_file_location(name, ROOT / EXAMPLES / (name + ".py"))
        scenario = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(scenario)

        fixture_roots = []

        def export(host):
            fixture_roots.append(host.root)
            raw_path, clean_path = host.root / "private.trace.json", host.root / "review.trace.json"
            host.export_trace(raw_path)
            original = json.loads(raw_path.read_text())["bundle"]
            host.export_trace(clean_path, through=original["through"], omit=sorted(original["artifacts"]))
            sanitized = validate_bundle(clean_path)
            assert sanitized["records"] == original["records"], "rewritten journal events"
            assert sanitized["expected"] == original["expected"], "rewritten replay identities"
            assert sanitized["artifacts"].keys() <= original["artifacts"].keys()
            replay = assert_replay(binary, clean_path, name, env=host.env)
            assert_false_exact_rejected(binary, clean_path, env=host.env)
            body = clean_path.read_bytes()
            (directory / (name + ".trace.json")).write_bytes(body)
            index["exporter_revision"] = sanitized["exporter_revision"]
            index["scenarios"][name] = {"sha256": sha256(body), "bytes": len(body),
                                         "records": len(sanitized["records"]), "replay": replay,
                                         "original_exact": original["exact"],
                                         "original_digest": json.loads(raw_path.read_text())["digest"],
                                         "records_sha256": sha256(encoded(original["records"]))}

        scenario.main(str(binary), export=export)
        assert len(fixture_roots) == 1 and not fixture_roots[0].exists(), "private fixture was not cleaned up"
    assert index["sources"] == source_hashes(ROOT) and index["runtime_sha256"] == runtime_hash(ROOT), \
        "sources changed during generation"
    (directory / "index.json").write_text(json.dumps(index, indent=2) + "\n")
    errors = check(ROOT, directory, binary)
    assert not errors, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generate", action="store_true", help="run all scenarios, including real Docker")
    parser.add_argument("--binary", type=Path, help="required for generation; validates real replay on check")
    parser.add_argument("--output", type=Path, default=ROOT / TRACES)
    args = parser.parse_args()
    if args.generate:
        if args.binary is None:
            parser.error("--generate requires --binary")
        generate(args.binary, args.output.resolve())
    else:
        errors = check(ROOT, args.output.resolve(), args.binary.resolve() if args.binary else None)
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
    print("Checked five sanitized non-exact fixture trace bundles; no upload.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
