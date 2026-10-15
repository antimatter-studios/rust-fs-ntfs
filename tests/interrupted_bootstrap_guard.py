"""Mutation checks for the bootstrap guard, without a driver or external tools."""

import gzip
import importlib.util
import struct
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "guard", ROOT / "scripts/check-interrupted-bootstrap.py"
)
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)


class BootstrapGuard(unittest.TestCase):
    def test_each_bootstrap_region_and_structural_failure_is_rejected(self):
        clean = bytearray(17 * 512)
        clean[:3] = b"\xeb\x52\x90"
        clean[3:11] = b"NTFS    "
        struct.pack_into("<H", clean, 11, 512)
        struct.pack_into("<Q", clean, 40, 16)
        clean[510:512] = b"\x55\xaa"
        clean[8192:] = clean[:512]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.img.gz"
            path.write_bytes(gzip.compress(clean))
            GUARD.check(path)
            for offset in [
                84,
                509,
                512,
                8191,
                8192 + 84,
                8192 + 509,
                510,
                8192 + 510,
                3,
            ]:
                with self.subTest(offset=offset):
                    bad = bytearray(clean)
                    bad[offset] ^= 1
                    path.write_bytes(gzip.compress(bad))
                    with self.assertRaises(AssertionError):
                        GUARD.check(path)
            for truncated in [clean[:8191], clean[:-1], clean + b"trailing"]:
                path.write_bytes(gzip.compress(truncated))
                with self.assertRaises(AssertionError):
                    GUARD.check(path)


if __name__ == "__main__":
    unittest.main()
