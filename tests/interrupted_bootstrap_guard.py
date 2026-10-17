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
            # An uncompressed volume, as the fuzz corpus commits them, is
            # held to the same regions.
            plain = Path(directory) / "volume.img"
            plain.write_bytes(clean)
            GUARD.check(plain)
            for offset in [84, 512, 8192 + 84]:
                with self.subTest(plain=offset):
                    bad = bytearray(clean)
                    bad[offset] ^= 1
                    plain.write_bytes(bad)
                    with self.assertRaises(AssertionError):
                        GUARD.check(plain)

    def test_a_boot_sector_seed_with_boot_code_is_rejected(self):
        clean = bytearray(512)
        clean[3:11] = b"NTFS    "
        clean[510:512] = b"\x55\xaa"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "seed.bin"
            path.write_bytes(clean)
            GUARD.check_boot_sector(path)
            for offset in [84, 509]:
                with self.subTest(offset=offset):
                    bad = bytearray(clean)
                    bad[offset] = 1
                    path.write_bytes(bad)
                    with self.assertRaises(AssertionError):
                        GUARD.check_boot_sector(path)
            for wrong in [clean[:511], clean + b"\0", bytes(512)]:
                path.write_bytes(wrong)
                with self.assertRaises(AssertionError):
                    GUARD.check_boot_sector(path)

    def test_embedded_data_holding_a_boot_sector_or_boot_message_is_rejected(self):
        sector = bytearray(512)
        sector[3:11] = b"NTFS    "
        sector[510:512] = b"\x55\xaa"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "table.bin"
            path.write_bytes(bytes(range(256)) * 8)
            GUARD.check_embedded(path)
            for bad in [bytes(1024) + sector] + [
                bytes(100) + message for message in GUARD.BOOT_MESSAGES
            ]:
                with self.subTest(bad=bytes(bad[-40:])):
                    path.write_bytes(bad)
                    with self.assertRaises(AssertionError):
                        GUARD.check_embedded(path)


if __name__ == "__main__":
    unittest.main()
