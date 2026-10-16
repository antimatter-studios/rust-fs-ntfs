"""The Python directory iterator agrees with the compiled C ABI.

Inputs, all required (a missing one fails, it never skips):
  FS_NTFS_SHARED_LIB  absolute path of the cdylib built by
                      `cargo rustc --release --lib --crate-type cdylib`
  FS_NTFS_DIR_PROBE   tests/python/abi_dir_listing.c compiled against
                      include/fs_ntfs.h and the same library
  FS_NTFS_FIXTURE     a decompressed NTFS image

This compares the ctypes client with an independently compiled C caller on
the same image. Both read through the same driver, so it proves the binding,
not the format reading: names and types graded against a Windows-authored
record of the same image remain owed.
"""

import ctypes
import hashlib
import os
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "python"))
import fs_ntfs_volume_info as fvi


def required(name):
    value = os.environ.get(name)
    if not value:
        raise AssertionError(f"{name} is not set; the CI job python-abi provides it")
    return value


LIB_PATH = required("FS_NTFS_SHARED_LIB")
PROBE = required("FS_NTFS_DIR_PROBE")
FIXTURE = required("FS_NTFS_FIXTURE")
MFT_ROOT_RECORD = 5


def run_probe(*args):
    out = subprocess.run([PROBE, *args], check=True, capture_output=True)
    fields, entries = {}, []
    for line in out.stdout.decode("utf-8").splitlines():
        if line.startswith("entry\t"):
            _, frn, ftype, nlen, name = line.split("\t", 4)
            entries.append((name, int(frn), int(ftype), int(nlen)))
        else:
            key, value = line.split("=", 1)
            fields[key] = value
    return fields, entries


class DirentLayout(unittest.TestCase):
    def test_layout_matches_the_c_compiler(self):
        fields, _ = run_probe()
        self.assertEqual(int(fields["sizeof"]), ctypes.sizeof(fvi.FsNtfsDirent))
        for name, *_ in fvi.FsNtfsDirent._fields_:
            self.assertEqual(
                int(fields[f"offset.{name}"]),
                getattr(fvi.FsNtfsDirent, name).offset,
                name,
            )


class DirListing(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = fvi.load_library(LIB_PATH)

    def test_root_listing_matches_the_c_caller(self):
        # #499 deliberately hides metafiles from the host-facing C ABI.
        # Independently prove that this fixture still contains $MFT at record
        # zero, and compare the visible records with ntfs-3g's native listing.
        # --system turns off ntfsls's implied --all, and without --all it drops
        # every POSIX-namespace name: this fixture's d000..d063 and the "." and
        # ".." it synthesises. --all restores them; DOS-only aliases stay out.
        before = hashlib.sha256(Path(FIXTURE).read_bytes()).hexdigest()
        reference = subprocess.run(
            ["ntfsls", "--system", "--all", "--inode", "--path", "/", FIXTURE],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        names = {}
        for line in reference.splitlines():
            ordinal, name = line.strip().split(maxsplit=1)
            self.assertNotIn(name, names)
            names[name] = fvi.DirEntry(name, int(ordinal), 0)
        self.assertIn("$MFT", names)
        self.assertEqual(names["$MFT"].file_record_number, 0)
        print(f"ntfsls raw root; image sha256={before}\n{reference}")
        fields, c_entries = run_probe(FIXTURE, "/")
        self.assertEqual(fields["end_errno"], "0")
        self.assertEqual(fields["skipped"], "0")
        with fvi.Volume(self.lib, FIXTURE) as vol:
            entries = vol.list_dir("/")
        py = [
            (e.name, e.file_record_number, e.file_type, len(e.name.encode("utf-8")))
            for e in entries
        ]
        self.assertEqual(py, c_entries)
        visible = {e.name: e.file_record_number for e in entries}
        expected = {
            name: e.file_record_number
            for name, e in names.items()
            if e.file_record_number >= 16
        }
        expected.update({".": MFT_ROOT_RECORD, "..": MFT_ROOT_RECORD})
        self.assertEqual(visible, expected)
        self.assertNotIn("$MFT", visible)
        self.assertNotIn("not-an-entry", visible)
        self.assertEqual(hashlib.sha256(Path(FIXTURE).read_bytes()).hexdigest(), before)

    def test_missing_path_raises_with_the_c_errno(self):
        fields, _ = run_probe(FIXTURE, "/no-such-directory-here")
        with (
            fvi.Volume(self.lib, FIXTURE) as vol,
            self.assertRaises(fvi.NtfsError) as caught,
        ):
            vol.open_dir("/no-such-directory-here")
        self.assertEqual(caught.exception.errno, int(fields["open_errno"]))
        self.assertNotEqual(caught.exception.errno, 0)

    def test_entries_are_copies_that_survive_next_and_close(self):
        with fvi.Volume(self.lib, FIXTURE) as vol:
            it = vol.open_dir("/")
            first = next(it)
            snapshot = (first.name, first.file_record_number, first.file_type)
            rest = list(it)
            it.close()
            self.assertTrue(rest)
            self.assertEqual(
                (first.name, first.file_record_number, first.file_type), snapshot
            )

    def test_close_is_idempotent_and_use_after_close_raises(self):
        with fvi.Volume(self.lib, FIXTURE) as vol:
            it = vol.open_dir("/")
            it.close()
            it.close()
            self.assertTrue(it.closed)
            with self.assertRaises(fvi.ClosedVolumeError):
                next(it)
            with self.assertRaises(fvi.ClosedVolumeError):
                it.skipped()

    def test_volume_close_closes_open_iterators_first(self):
        vol = fvi.Volume(self.lib, FIXTURE)
        it = vol.open_dir("/")
        vol.close()
        self.assertTrue(it.closed)
        with self.assertRaises(fvi.ClosedVolumeError):
            vol.open_dir("/")
        it.close()

    def test_embedded_nul_path_is_refused(self):
        with fvi.Volume(self.lib, FIXTURE) as vol, self.assertRaises(ValueError):
            vol.open_dir("/a\0b")

    def test_skipped_entries_are_never_presented_as_complete(self):
        err = fvi.IncompleteListingError("/", 2, [])
        self.assertEqual(err.skipped, 2)
        self.assertIsInstance(err, RuntimeError)


if __name__ == "__main__":
    unittest.main()
