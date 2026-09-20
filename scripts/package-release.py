#!/usr/bin/env python3
"""Create a reproducible gzip-compressed tar archive."""

from __future__ import annotations

import argparse
import gzip
import tarfile
from pathlib import Path


def _member(path: Path, archive_name: str, epoch: int) -> tarfile.TarInfo:
    info = tarfile.TarInfo(archive_name)
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = epoch
    if path.is_dir():
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
    elif path.is_file():
        info.type = tarfile.REGTYPE
        info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        info.size = path.stat().st_size
    else:
        raise ValueError(f"release archive source must be a regular file or directory: {path}")
    return info


def create_archive(source: Path, output: Path, epoch: int) -> None:
    if not source.is_dir():
        raise ValueError(f"release archive source is not a directory: {source}")
    paths = [source, *sorted(source.rglob("*"), key=lambda path: path.relative_to(source).as_posix())]
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                for path in paths:
                    relative = path.relative_to(source.parent).as_posix()
                    info = _member(path, relative, epoch)
                    if path.is_file():
                        with path.open("rb") as content:
                            archive.addfile(info, content)
                    else:
                        archive.addfile(info)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--epoch", type=int, required=True)
    arguments = parser.parse_args()
    if arguments.epoch < 0:
        parser.error("--epoch must be a non-negative Unix timestamp")
    create_archive(arguments.source, arguments.output, arguments.epoch)


if __name__ == "__main__":
    main()
