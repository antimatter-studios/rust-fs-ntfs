#!/usr/bin/env python3
"""Refuse formatter bootstrap code in every committed NTFS image (#487, #504).

A formatter copies its own boot code onto every volume it makes: Windows
`format` writes Microsoft's, and mkntfs writes its own, from GPL source.
Neither this crate nor chkdsk reads it, and Windows does not run it on a
data volume, so no committed image carries it. Four inventories:

- the registered interrupted-write partitions, each a gzipped partition
  named in tests/windows_interrupted_fixture.rs;
- every other tracked `*.img` or `*.img.gz` whose first sector is an NTFS
  boot sector, which is every whole volume committed for a test or a fuzz
  seed;
- the boot-sector seeds under fuzz/corpus/boot_sector;
- the data files under src/, which the published crate embeds: none may
  hold an NTFS boot sector or a formatter's boot message.

Each inventory has a floor, so a glob that stops matching fails rather than
checking nothing.
"""

import gzip
import re
import struct
import subprocess
from pathlib import Path

# The messages each formatter's boot code prints. Windows `format`:
# "A disk read error occurred", "BOOTMGR is compressed", "Press
# Ctrl+Alt+Del to restart". mkntfs: "This is not a bootable disk".
BOOT_MESSAGES = [
    b"A disk read error occurred",
    b"BOOTMGR is",
    b"Press Ctrl+Alt+Del",
    b"This is not a bootable disk",
]


def open_image(path):
    return gzip.open(path, "rb") if path.name.endswith(".gz") else open(path, "rb")


def is_ntfs_boot_sector(sector):
    return sector[3:11] == b"NTFS    " and sector[510:512] == b"\x55\xaa"


def check(path):
    with open_image(path) as image:
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


def check_boot_sector(path):
    data = path.read_bytes()
    assert len(data) == 512, f"{path}: {len(data)} bytes, not one 512-byte boot sector"
    assert is_ntfs_boot_sector(data), f"{path}: not an NTFS boot sector"
    assert not any(data[84:510]), (
        f"{path}: nonzero boot code ({sum(b != 0 for b in data[84:510])} bytes)"
    )


def check_embedded(path):
    data = path.read_bytes()
    for message in BOOT_MESSAGES:
        assert message not in data, (
            f"{path}: holds a formatter's boot message {message!r}"
        )
    for at in range(0, len(data) - 511, 512):
        assert not is_ntfs_boot_sector(data[at : at + 512]), (
            f"{path}: holds an NTFS boot sector at byte {at}"
        )


def tracked(root):
    listing = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"], check=True, capture_output=True
    ).stdout
    return sorted(root / name.decode() for name in listing.split(b"\0") if name)


def first_sector(path):
    with open_image(path) as image:
        return image.read(512)


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    source = (root / "tests/windows_interrupted_fixture.rs").read_text()
    names = re.findall(r'"(windows-interrupted-[^"\n]+)",\s*"[0-9a-f]{64}"', source)
    paths = sorted((root / "test-disks").glob("windows-interrupted-*.img.gz"))
    assert len(names) >= 26, "interrupted fixture inventory shrank below 26"
    assert {p.name for p in paths} == {n + ".img.gz" for n in names}, (
        "missing or unregistered interrupted fixture"
    )
    files = tracked(root)
    images = [
        p
        for p in files
        if p.name.endswith((".img", ".img.gz"))
        and not p.name.startswith("windows-interrupted-")
        and is_ntfs_boot_sector(first_sector(p))
    ]
    seeds = [p for p in files if p.parent == root / "fuzz/corpus/boot_sector"]
    embedded = [
        p for p in files if p.is_relative_to(root / "src") and p.suffix != ".rs"
    ]
    assert len(images) >= 3, (
        f"only {len(images)} other NTFS images found; expected at least 3"
    )
    assert len(seeds) >= 2, (
        f"only {len(seeds)} boot-sector seeds found; expected at least 2"
    )
    assert len(embedded) >= 2, (
        f"only {len(embedded)} embedded data files found; expected at least 2"
    )
    failures = []
    for checker, inventory in [
        (check, paths),
        (check, images),
        (check_boot_sector, seeds),
        (check_embedded, embedded),
    ]:
        for path in inventory:
            try:
                checker(path)
            except (AssertionError, OSError, EOFError) as error:
                failures.append(str(error))
    assert not failures, "\n".join(failures)
    print(
        f"bootstrap: {len(paths)} interrupted partitions, {len(images)} other images, "
        f"{len(seeds)} boot-sector seeds and {len(embedded)} embedded files checked"
    )
