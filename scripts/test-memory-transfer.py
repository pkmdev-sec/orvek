#!/usr/bin/env python3
"""Prove self-import and multi-hop archive roundtrips through the real CLI, without a provider."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="orvek-transfer-") as temporary:
        root = Path(temporary)
        configs = {}
        for name in ("a", "b", "c"):
            directory = root / name
            directory.mkdir()
            config = directory / "config.toml"
            config.write_text("[memory]\nenabled = true\n[skills]\nenabled = false\n")
            config.chmod(0o600)
            configs[name] = config
        database = root / "a/memory/v1.sqlite3"
        database.parent.mkdir()
        with sqlite3.connect(database) as connection:
            connection.executescript("""CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, normalized_identity TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
                last_scanned_at_ms INTEGER, scan_count INTEGER NOT NULL DEFAULT 0,
                last_used_at_ms INTEGER, use_count INTEGER NOT NULL DEFAULT 0,
                probation_until_ms INTEGER, version INTEGER NOT NULL DEFAULT 1);
                INSERT INTO memories(id,content,normalized_identity,created_at_ms,updated_at_ms)
                VALUES (1,'portable preference','portable preference',1,1);
                PRAGMA user_version=1;""")
        environment = {key: value for key, value in os.environ.items() if not key.startswith(("ORVEK_", "TACT_"))}
        environment.update(HOME=str(root), CODEX_HOME=str(root / "codex"))

        def run(owner, operation, archive):
            command = [str(binary), "--config", str(configs[owner]), "memory", operation, str(root / archive)]
            result = subprocess.run(command, env=environment, cwd=root, capture_output=True, text=True, timeout=30)
            print(f"{owner}: memory {operation} {archive}: exit={result.returncode}: {result.stdout.strip()}")
            assert result.returncode == 0, result.stderr

        def record(owner):
            with sqlite3.connect(root / owner / "memory/v1.sqlite3") as connection:
                records = list(connection.execute("SELECT content, version, metadata FROM memories"))
            assert len(records) == 1, records
            return records[0][0], records[0][1], json.loads(records[0][2])

        run("a", "export", "first")
        original = record("a")
        run("a", "import", "first")
        assert record("a")[0] == original[0]
        run("a", "export", "second")
        assert len(json.loads((root / "second/manifest.json").read_text())["records"]) == 1
        run("b", "import", "second")
        run("b", "export", "third")
        run("c", "import", "third")
        run("c", "export", "fourth")
        run("a", "import", "fourth")
        merged = record("a")
        assert merged[0] == original[0]
        owners = {source["ownership_id"] for source in merged[2]["transferred_from"]}
        assert len(owners) == 3 and original[2]["ownership_id"] in owners
        run("a", "import", "fourth")
        assert record("a") == merged, "repeat import must not mutate version or provenance"
        print("PASS: real CLI legacy migration, self-import, re-export, A→B→C→A provenance, idempotent replay")


if __name__ == "__main__":
    main()
