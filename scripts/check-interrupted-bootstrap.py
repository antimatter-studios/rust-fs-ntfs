#!/usr/bin/env python3
"""Refuse formatter bootstrap code in every interrupted-write partition (#487)."""

import gzip
import re
import struct
from pathlib import Path


def check(path):
    with gzip.open(path, "rb") as image:
        boot = image.read(8192)
        assert len(boot) == 8192, f"{path}: truncated $Boot"
        assert boot[3:11] == b"NTFS    ", f"{path}: missing NTFS BPB"
        sector = struct.unpack_from("<H", boot, 11)[0]
        assert sector == 512, f"{path}: unexpected sector size {sector}"
        assert boot[510:512] == b"\x55\xaa", f"{path}: missing boot signature"
        last = struct.unpack_from("<Q", boot, 40)[0] * sector
        assert last >= 8192, f"{path}: invalid backup boot offset"
        image.seek(last)
        backup = image.read(sector)
        assert len(backup) == sector, f"{path}: missing backup boot sector"
        assert not image.read(1), f"{path}: unexpected bytes after backup boot sector"
        assert backup[:84] == boot[:84], f"{path}: backup BPB differs"
        assert backup[510:512] == b"\x55\xaa", f"{path}: missing backup signature"
        for name, data in [
            ("boot code", boot[84:510]),
            ("$Boot sectors 1-15", boot[512:8192]),
            ("backup boot code", backup[84:510]),
        ]:
            assert not any(data), (
                f"{path}: nonzero {name} ({sum(b != 0 for b in data)} bytes)"
            )


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    source = (root / "tests/windows_interrupted_fixture.rs").read_text()
    names = re.findall(r'"(windows-interrupted-[^"\n]+)",\s*"[0-9a-f]{64}"', source)
    paths = sorted((root / "test-disks").glob("windows-interrupted-*.img.gz"))
    assert len(names) >= 26, "interrupted fixture inventory shrank below 26"
    assert {p.name for p in paths} == {n + ".img.gz" for n in names}, (
        "missing or unregistered interrupted fixture"
    )
    failures = []
    for path in paths:
        try:
            check(path)
        except (AssertionError, OSError, EOFError) as error:
            failures.append(str(error))
    assert not failures, "\n".join(failures)
    print(f"interrupted bootstrap: {len(paths)} partitions checked")
