"""Same-image Windows oracle; no expected IDs come from this driver.

The ABI exposes an MFT ordinal, whereas Windows queryfileid reports a full
FILE_REFERENCE. Validate the sequence independently using Windows' retained
FSCTL_GET_NTFS_FILE_RECORD bytes before comparing the ordinal. No ID is masked
to make a comparison pass. See docs/testing/python-directory-oracle.md.
"""

import copy
import json
import os
import re
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "python"))
import fs_ntfs_volume_info as fvi

sys.path.insert(0, os.path.dirname(__file__))
from record_binding import binding_errors, sha256


def required(name):
    value = os.environ.get(name)
    if not value:
        raise AssertionError(f"{name} missing; run the Windows fixture CI route")
    return value


def native_ordinal(entry):
    native_text = entry["native_id"]
    if not re.fullmatch(r"0x[0-9a-fA-F]{1,32}", native_text):
        raise AssertionError("invalid native ID")
    if re.findall(r"0x[0-9a-fA-F]+", entry["queryfileid_output"]) != [native_text]:
        raise AssertionError("native ID differs from retained fsutil output")
    native = int(native_text, 16)
    if not 0 < native < (1 << 64):
        raise AssertionError("native ID is not a 64-bit NTFS reference")
    sequence, ordinal = divmod(native, 1 << 48)
    raw = bytes.fromhex(entry["fsctl_output_hex"])
    if len(raw) < 60:
        raise AssertionError("short Windows FSCTL output")
    returned = int.from_bytes(raw[:8], "little")
    length = int.from_bytes(raw[8:12], "little")
    record = raw[12 : 12 + length]
    # The native output structure has trailing alignment padding on some
    # platforms; its explicit FileRecordLength bounds the record bytes.
    if not 12 + length <= len(raw) <= 16 + length or record[:4] != b"FILE":
        raise AssertionError("invalid Windows FILE record")
    # FSCTL can return a LOWER ordinal: exact equality is essential.
    if returned != ordinal:
        raise AssertionError("Windows returned a different MFT ordinal")
    if not sequence or int.from_bytes(record[16:18], "little") != sequence:
        raise AssertionError("native sequence differs from Windows FILE header")
    directory = bool(entry["attributes"] & 0x10)
    if directory != entry["directory"]:
        raise AssertionError("Windows enumeration attributes disagree")
    if bool(int.from_bytes(record[22:24], "little") & 2) != directory:
        raise AssertionError("Windows FILE header type disagrees")
    return ordinal


def expected_entries(listing):
    result = {}
    for entry in listing["entries"]:
        name = entry["name"]
        if name in result or "/" in name:
            raise AssertionError("invalid or duplicate controlled name")
        result[name] = (native_ordinal(entry), 2 if entry["directory"] else 1)
    return result


def compare_listing(listing, actual):
    expected = expected_entries(listing)
    if len(actual) != len({name for name, _, _ in actual}):
        raise AssertionError("duplicate ABI entry")
    observed = {name: (ordinal, kind) for name, ordinal, kind in actual}
    if observed != expected:
        raise AssertionError(f"{listing['path']}: {observed!r} != Windows {expected!r}")


class DirectoryMatchesWindows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.image = required("FS_NTFS_WINDOWS_IMAGE")
        cls.head = required("FS_NTFS_EXPECTED_HEAD")
        cls.probe = required("FS_NTFS_DIR_PROBE")
        cls.lib = fvi.load_library(required("FS_NTFS_SHARED_LIB"))
        with open(required("FS_NTFS_DIRECTORY_RECORD"), encoding="utf-8-sig") as stream:
            cls.record = json.load(stream)
        if binding_errors(cls.record, cls.image, cls.head):
            raise AssertionError("Windows directory record binding failed")
        if cls.record["schema"] != 1:
            raise AssertionError("unknown directory oracle schema")

    def test_python_and_c_match_windows_for_every_controlled_directory(self):
        before = sha256(self.image)
        listings = self.record["listings"]
        self.assertEqual(
            {x["path"] for x in listings},
            {"/oracle-dir", "/oracle-dir/empty", "/oracle-dir/dir-日本"},
        )
        root = next(x for x in listings if x["path"] == "/oracle-dir")
        self.assertEqual(
            set(expected_entries(root)),
            {".", "..", "ascii.txt", "naïve-日本.txt", "empty", "dir-日本"},
        )
        self.assertEqual(len(listings), 3)
        for listing in listings:
            if listing["path"] != "/oracle-dir":
                self.assertEqual(set(expected_entries(listing)), {".", ".."})
        with fvi.Volume(self.lib, self.image) as volume:
            for listing in listings:
                with self.subTest(path=listing["path"]):
                    py = volume.list_dir(listing["path"])
                    actual = [(e.name, e.file_record_number, e.file_type) for e in py]
                    compare_listing(listing, actual)
                    output = subprocess.run(
                        [self.probe, self.image, listing["path"]],
                        check=True,
                        capture_output=True,
                        text=True,
                    ).stdout
                    entries, fields = [], {}
                    for line in output.splitlines():
                        if line.startswith("entry\t"):
                            _, ordinal, kind, size, name = line.split("\t", 4)
                            self.assertEqual(int(size), len(name.encode("utf-8")))
                            entries.append((name, int(ordinal), int(kind)))
                        else:
                            key, value = line.split("=", 1)
                            fields[key] = value
                    self.assertEqual(fields["end_errno"], "0")
                    self.assertEqual(fields["skipped"], "0")
                    compare_listing(listing, entries)
                    self.assertEqual(sorted(actual), sorted(entries))
        self.assertEqual(sha256(self.image), before)

    def test_wrong_image_and_head_are_rejected_by_acceptance_rule(self):
        wrong = required("FS_NTFS_WRONG_IMAGE")
        self.assertIn("image sha256", binding_errors(self.record, wrong, self.head))
        wrong_head = ("0" if self.head[0] != "0" else "1") + self.head[1:]
        self.assertIn("git HEAD", binding_errors(self.record, self.image, wrong_head))

    def test_changed_record_names_types_ids_and_sequences_are_rejected(self):
        listing = next(x for x in self.record["listings"] if x["path"] == "/oracle-dir")
        actual = [
            (e["name"], native_ordinal(e), 2 if e["directory"] else 1)
            for e in listing["entries"]
        ]
        for field in ("name", "directory", "native_id", "fsctl_output_hex"):
            changed = copy.deepcopy(listing)
            entry = changed["entries"][0]
            if field == "name":
                entry[field] += "-wrong"
            elif field == "directory":
                entry[field] = not entry[field]
            elif field == "native_id":
                # Change ONLY sequence bits: dropping them would hide this.
                entry[field] = hex(int(entry[field], 16) ^ (1 << 48))
                entry["queryfileid_output"] = entry[field]
            else:
                raw = bytearray.fromhex(entry[field])
                raw[0] ^= 1  # Windows returned a different ordinal.
                entry[field] = raw.hex()
            with self.subTest(field=field), self.assertRaises(AssertionError):
                compare_listing(changed, actual)


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(DirectoryMatchesWindows)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if result.skipped or result.testsRun != 3 or not result.wasSuccessful():
        raise SystemExit(1)
