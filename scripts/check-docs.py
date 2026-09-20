#!/usr/bin/env python3
"""Check first-party Markdown links and configuration examples without running them."""

from pathlib import Path
import json
import re
import subprocess
import sys
import tomllib
from urllib.parse import unquote, urlsplit

root = Path(__file__).resolve().parent.parent
names = subprocess.check_output(["git", "ls-files", "*.md"], cwd=root, text=True).splitlines()
failures = []
checked = 0


def anchors(text):
    result = set()
    counts = {}
    for heading in re.findall(r"^#{1,6}\s+(.+)$", text, flags=re.MULTILINE):
        slug = re.sub(r"[^\w\s-]", "", heading.replace("`", "").lower()).replace(" ", "-")
        count = counts.get(slug, 0)
        counts[slug] = count + 1
        result.add(f"{slug}-{count}" if count else slug)
    return result


for name in names:
    path = root / name
    if name.startswith(("vendor/", ".codex/")) or name == "LICENSE.md" or not path.is_file():
        continue
    text = path.read_text()
    checked += 1
    for language, body in re.findall(r"^```([^\n]*)\n(.*?)^```\s*$", text, flags=re.MULTILINE | re.DOTALL):
        language = language.strip()
        try:
            if language == "toml":
                tomllib.loads(body)
            elif language == "json":
                json.loads(body)
            elif language in {"sh", "bash", "shell"}:
                result = subprocess.run(["bash", "-n"], input=body, text=True, capture_output=True)
                if result.returncode:
                    raise ValueError(result.stderr.strip())
        except ValueError as error:
            failures.append(f"{name}: invalid {language} example: {error}")
    prose = re.sub(r"^```[^\n]*\n.*?^```\s*$", "", text, flags=re.MULTILINE | re.DOTALL)
    for target in re.findall(r"\[[^\]]*\]\(([^)\s]+)\)", prose):
        link = urlsplit(target)
        if link.scheme or link.netloc:
            continue
        destination = path.parent / unquote(link.path) if link.path else path
        if not destination.exists():
            failures.append(f"{name}: missing local link {target}")
        elif link.fragment and destination.suffix == ".md":
            if unquote(link.fragment) not in anchors(destination.read_text()):
                failures.append(f"{name}: missing heading {target}")
if failures:
    print("\n".join(failures), file=sys.stderr)
    raise SystemExit(1)
print(f"Checked {checked} first-party Markdown files: local links and TOML/JSON/shell examples.")
