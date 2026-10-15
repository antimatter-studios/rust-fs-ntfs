"""Compare scrubbed fixtures with their original git blobs, byte for byte."""

import gzip
import hashlib
import json
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class BootstrapPreservation(unittest.TestCase):
    def test_every_byte_outside_the_three_bootstrap_regions_is_retained(self):
        evidence = json.loads(
            (ROOT / "test-disks/windows-interrupted-bootstrap.json").read_text()
        )
        self.assertEqual(len(evidence["partitions"]), 26)
        for row in evidence["partitions"]:
            with self.subTest(image=row["name"]):
                original = gzip.decompress(
                    subprocess.check_output(
                        [
                            "git",
                            "show",
                            f"{evidence['original_commit']}:test-disks/{row['name']}",
                        ],
                        cwd=ROOT,
                    )
                )
                self.assertEqual(
                    hashlib.sha256(original).hexdigest(), row["original_sha256"]
                )
                current = gzip.decompress(
                    (ROOT / "test-disks" / row["name"]).read_bytes()
                )
                self.assertEqual(len(current), row["size"])
                self.assertEqual(
                    hashlib.sha256(current).hexdigest(), row["scrubbed_sha256"]
                )
                expected = bytearray(original)
                backup = row["backup_offset"]
                unaffected = hashlib.sha256()
                at = 0
                counts = []
                for lo, hi in [(84, 510), (512, 8192), (backup + 84, backup + 510)]:
                    unaffected.update(original[at:lo])
                    at = hi
                    counts.append(sum(b != 0 for b in original[lo:hi]))
                    expected[lo:hi] = bytes(hi - lo)
                unaffected.update(original[at:])
                self.assertEqual(unaffected.hexdigest(), row["unchanged_sha256"])
                self.assertEqual(counts, row["changed_bytes"])
                self.assertTrue(
                    current == expected, "bytes outside bootstrap regions changed"
                )


if __name__ == "__main__":
    unittest.main()
