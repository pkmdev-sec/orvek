#!/usr/bin/env python3
"""Rebuild the bundled printable-ASCII subset from the pinned public-domain BDF."""

import argparse
import hashlib
from pathlib import Path
from urllib.request import urlopen

SOURCE_URL = (
    "https://raw.githubusercontent.com/can1357/oh-my-pi/"
    "e109c5a63fc1ef67e8543094be45494de1252cf4/"
    "crates/pi-natives/src/fonts/8x13.bdf"
)
SOURCE_SHA256 = "c39b53bd625f35e6681d5097d81e6bf81b44a9f6730068c4c8f4716d779fd40e"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, help="Use a local copy of the pinned BDF")
    args = parser.parse_args()
    if args.source:
        source = args.source.read_bytes()
    else:
        with urlopen(SOURCE_URL, timeout=30) as response:
            source = response.read()
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise SystemExit("The source BDF does not match the pinned SHA-256")

    glyphs = {}
    for glyph in source.decode("ascii").split("STARTCHAR ")[1:]:
        lines = glyph.splitlines()
        codepoint = int(next(line for line in lines if line.startswith("ENCODING ")).split()[1])
        if not 32 <= codepoint <= 126:
            continue
        if "BBX 8 13 0 -2" not in lines or "DWIDTH 8 0" not in lines:
            raise SystemExit(f"Unexpected metrics for glyph {codepoint}")
        rows = lines[lines.index("BITMAP") + 1 : lines.index("ENDCHAR")]
        if len(rows) != 13 or any(len(row) != 2 for row in rows):
            raise SystemExit(f"Unexpected bitmap for glyph {codepoint}")
        glyphs[codepoint] = bytes.fromhex("".join(rows))
    if set(glyphs) != set(range(32, 127)):
        raise SystemExit("The font does not contain all printable ASCII glyphs")

    output = b"".join(glyphs[codepoint] for codepoint in range(32, 127))
    target = Path(__file__).with_name("8x13-ascii.bin")
    target.write_bytes(output)
    print(f"{target.name}: {len(output)} bytes, sha256={hashlib.sha256(output).hexdigest()}")


if __name__ == "__main__":
    main()
