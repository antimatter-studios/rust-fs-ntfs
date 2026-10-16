"""The Python client's serial and geometry agree with Windows' own record.

Windows formats ntfs-cli-read.img in the windows-native-read-fixtures job and,
while the volume is mounted, records `fsutil fsinfo ntfsinfo` beside the
image's SHA-256. Nothing here derives an expected value from our reader.

Inputs, all required (a missing one fails, it never skips):
  FS_NTFS_SHARED_LIB      absolute path of the cdylib
  FS_NTFS_WINDOWS_IMAGE   test-disks/ntfs-cli-read.img from that job
  FS_NTFS_WINDOWS_RECORD  test-disks/ntfs-cli-read.ntfsinfo.txt from that job
  FS_NTFS_WRONG_IMAGE     a different Windows-formatted image (negative control)
  FS_NTFS_EXPECTED_HEAD   the revision the producer and this job checked out
"""

import hashlib
import os
import re
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "python"))
import fs_ntfs_volume_info as fvi

sys.path.insert(0, os.path.dirname(__file__))
from record_binding import binding_errors


def required(name):
    value = os.environ.get(name)
    if not value:
        raise AssertionError(
            f"{name} is not set; the CI job python-volume-info-windows provides it"
        )
    return value


LIB_PATH = required("FS_NTFS_SHARED_LIB")
IMAGE = required("FS_NTFS_WINDOWS_IMAGE")
RECORD = required("FS_NTFS_WINDOWS_RECORD")
WRONG_IMAGE = required("FS_NTFS_WRONG_IMAGE")
# The revision both jobs checked out: github.sha, which is the merge commit
# on a pull_request event and the pushed commit otherwise.
EXPECTED_HEAD = required("FS_NTFS_EXPECTED_HEAD")


def parse_record(text):
    """`key : value` lines of the record, keys as fsutil prints them."""
    fields = {}
    for line in text.splitlines():
        m = re.match(r"^\s*([^:]+?)\s*:\s*(\S+)", line)
        if m:
            fields.setdefault(m.group(1), m.group(2))
    return fields


def number(fields, key):
    if key not in fields:
        raise AssertionError(f"Windows record has no '{key}' line")
    return int(fields[key].replace(",", ""), 0)


def windows_expectations(fields):
    """Each exposed field mapped to its fsutil counterpart, with units."""
    major, minor = fields["NTFS Version"].split(".")
    bytes_per_sector = number(fields, "Bytes Per Sector")
    return {
        # 64-bit volume serial number from the boot sector, as printed.
        "serial_number": number(fields, "NTFS Volume Serial Number"),
        # bytes
        "bytes_per_sector": bytes_per_sector,
        # bytes
        "cluster_size": number(fields, "Bytes Per Cluster"),
        # clusters
        "total_clusters": number(fields, "Total Clusters"),
        # bytes: sectors x bytes per sector. The runner's fsutil prints
        # "Total Sectors"; older releases called it "Number Sectors".
        "total_size": number(fields, "Total Sectors") * bytes_per_sector,
        # bytes
        "mft_record_size": number(fields, "Bytes Per FileRecord Segment"),
        "ntfs_version_major": int(major),
        "ntfs_version_minor": int(minor),
    }


def mismatches(info, expected):
    return {k: (info[k], v) for k, v in expected.items() if info[k] != v}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def volume_info(lib, path):
    with fvi.Volume(lib, path) as vol:
        info = vol.volume_info()
    # The client returns the version as one (major, minor) pair; fsutil
    # prints one "3.1" field, compared here part by part.
    info["ntfs_version_major"], info["ntfs_version_minor"] = info["ntfs_version"]
    return info


class VolumeInfoMatchesWindows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = fvi.load_library(LIB_PATH)
        with open(RECORD, encoding="utf-8-sig") as f:
            cls.fields = parse_record(f.read())
        cls.expected = windows_expectations(cls.fields)

    def test_record_is_bound_to_this_image(self):
        self.assertEqual(binding_errors(self.fields, IMAGE, EXPECTED_HEAD), [])

    def test_wrong_head_is_refused_by_the_binding(self):
        # A well-formed 40-hex revision that is not the one checked out.
        head = self.fields.get("git HEAD", "")
        wrong = ("0" if head[:1] != "0" else "1") + head[1:]
        self.assertIn("git HEAD", binding_errors(self.fields, IMAGE, wrong))

    def test_serial_and_geometry_match_fsutil(self):
        info = volume_info(self.lib, IMAGE)
        self.assertEqual(mismatches(info, self.expected), {})

    def test_wrong_image_is_refused_by_the_binding(self):
        self.assertNotEqual(self.fields.get("image sha256"), sha256(WRONG_IMAGE))

    def test_wrong_image_disagrees_with_the_record(self):
        info = volume_info(self.lib, WRONG_IMAGE)
        self.assertIn("serial_number", mismatches(info, self.expected))

    def test_changed_record_is_detected(self):
        info = volume_info(self.lib, IMAGE)
        for key in self.expected:
            changed = dict(self.expected, **{key: self.expected[key] + 1})
            self.assertIn(key, mismatches(info, changed), key)


if __name__ == "__main__":
    unittest.main()
