"""The Python ctypes client agrees with the compiled C ABI.

Inputs, all required (a missing one fails, it never skips):
  FS_NTFS_SHARED_LIB  absolute path of the cdylib built by
                      `cargo rustc --release --lib --crate-type cdylib`
  FS_NTFS_ABI_PROBE   tests/python/abi_volume_info.c compiled against
                      include/fs_ntfs.h and the same library
  FS_NTFS_FIXTURE     a decompressed NTFS image
"""

import ctypes
import errno
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "python"))
import fs_ntfs_volume_info as fvi


def required(name):
    value = os.environ.get(name)
    if not value:
        raise AssertionError(f"{name} is not set; the CI job python-abi provides it")
    return value


LIB_PATH = required("FS_NTFS_SHARED_LIB")
PROBE = required("FS_NTFS_ABI_PROBE")
FIXTURE = required("FS_NTFS_FIXTURE")


def probe(*args):
    out = subprocess.run([PROBE, *args], check=True, capture_output=True, text=True)
    return dict(line.split("=", 1) for line in out.stdout.splitlines())


def cluster_count_contract_holds(total_size, cluster_size, total_clusters, bps):
    """total_size is the boot sector's total sectors times bytes per sector;
    total_clusters counts only whole clusters, so a volume whose sector count
    is not a multiple of the cluster size keeps a partial tail cluster that
    total_size includes and total_clusters does not."""
    if cluster_size <= 0 or bps <= 0 or total_size % bps:
        return False
    return total_clusters == total_size // cluster_size


class ClusterCountContract(unittest.TestCase):
    def test_whole_and_partial_tail_clusters(self):
        self.assertTrue(cluster_count_contract_holds(8 * 4096, 4096, 8, 512))
        self.assertTrue(cluster_count_contract_holds(8 * 4096 + 512, 4096, 8, 512))
        self.assertTrue(cluster_count_contract_holds(9 * 4096 - 512, 4096, 8, 512))

    def test_counterexamples_are_refused(self):
        self.assertFalse(cluster_count_contract_holds(8 * 4096 + 512, 4096, 9, 512))
        self.assertFalse(cluster_count_contract_holds(9 * 4096, 4096, 8, 512))
        self.assertFalse(cluster_count_contract_holds(8 * 4096 + 1, 4096, 8, 512))
        self.assertFalse(cluster_count_contract_holds(8 * 4096, 0, 8, 512))


class VolumeInfoAbi(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lib = fvi.load_library(LIB_PATH)

    def test_layout_matches_c_compiler(self):
        c = probe()
        self.assertEqual(int(c["sizeof"]), ctypes.sizeof(fvi.FsNtfsVolumeInfoV2))
        for name, _ in fvi.FsNtfsVolumeInfoV2._fields_:
            self.assertEqual(
                int(c["offset." + name]),
                getattr(fvi.FsNtfsVolumeInfoV2, name).offset,
                name,
            )

    def test_values_match_c_caller(self):
        c = probe(FIXTURE)
        with fvi.Volume(self.lib, FIXTURE) as vol:
            info = vol.volume_info()
        self.assertEqual(info["volume_name"], c["volume_name"])
        for key in (
            "cluster_size",
            "total_clusters",
            "serial_number",
            "total_size",
            "volume_flags",
            "mft_record_size",
            "bytes_per_sector",
        ):
            self.assertEqual(info[key], int(c[key]), key)
        self.assertEqual(
            info["ntfs_version"],
            (int(c["ntfs_version_major"]), int(c["ntfs_version_minor"])),
        )
        self.assertEqual(info["is_dirty"], c["is_dirty"] == "1")
        self.assertTrue(
            cluster_count_contract_holds(
                info["total_size"],
                info["cluster_size"],
                info["total_clusters"],
                info["bytes_per_sector"],
            ),
            info,
        )
        self.assertIn(info["bytes_per_sector"], (512, 4096))
        self.assertIn(info["mft_record_size"], (1024, 4096))

    def test_volume_name_buffer_is_terminated(self):
        with fvi.Volume(self.lib, FIXTURE) as vol:
            buf = fvi.FsNtfsVolumeInfoV2()
            self.assertEqual(
                self.lib.fs_ntfs_get_volume_info_v2(vol._handle, ctypes.byref(buf)), 0
            )
        raw = ctypes.string_at(ctypes.addressof(buf), 128)
        self.assertIn(b"\0", raw)

    def test_null_arguments_report_einval(self):
        info = fvi.FsNtfsVolumeInfoV2()
        self.assertEqual(
            self.lib.fs_ntfs_get_volume_info_v2(None, ctypes.byref(info)), -1
        )
        self.assertEqual(self.lib.fs_ntfs_last_errno(), errno.EINVAL)
        self.assertTrue(self.lib.fs_ntfs_last_error())

    def test_missing_image_reports_error(self):
        with self.assertRaises(fvi.NtfsError) as caught:
            fvi.Volume(self.lib, "/nonexistent/fs-ntfs-python-abi.img")
        self.assertNotEqual(caught.exception.errno, 0)
        self.assertIn("fs_ntfs_mount", str(caught.exception))

    def test_embedded_nul_is_refused(self):
        with self.assertRaises(ValueError):
            fvi.Volume(self.lib, FIXTURE + "\0suffix")

    def test_relative_library_path_is_refused(self):
        with self.assertRaises(ValueError):
            fvi.load_library("libfs_ntfs.so")

    def test_close_is_idempotent_and_use_after_close_fails(self):
        vol = fvi.Volume(self.lib, FIXTURE)
        vol.close()
        vol.close()
        self.assertTrue(vol.closed)
        with self.assertRaises(fvi.ClosedVolumeError):
            vol.volume_info()

    def test_unicode_image_path(self):
        tmp = tempfile.mkdtemp()
        try:
            path = os.path.join(tmp, "volume-é中.img")
            shutil.copyfile(FIXTURE, path)
            with fvi.Volume(self.lib, FIXTURE) as a, fvi.Volume(self.lib, path) as b:
                self.assertEqual(a.volume_info(), b.volume_info())
        finally:
            os.remove(path)
            os.rmdir(tmp)


if __name__ == "__main__":
    unittest.main()
