#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(sys.argv[1])
SCRIPT = ROOT / "scripts/slate-lab-source.sh"
sys.argv = sys.argv[:1]


class LabSourceTests(unittest.TestCase):
    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.root = Path(self.workspace.name)
        self.source = self.root / "original.img"
        self.source.write_bytes(b"RSTR" + bytes(8188) + b"user file bytes")
        self.destination = self.root / "slate-write-lab-source.img"
        self.before = self.source.read_bytes()
        self.environment = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}")
        stub = '''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
tool = Path(sys.argv[0]).name
if tool == "ntfs-3g.probe":
    assert sys.argv[1] == "-w"
    sys.exit(int(os.environ.get("PROBE_STATUS", "0")))
if tool == "ntfscat":
    assert sys.argv[2] == "/$LogFile"
    if os.environ.get("NTFSCAT_STATUS"):
        sys.exit(int(os.environ["NTFSCAT_STATUS"]))
    sys.stdout.buffer.write(Path(sys.argv[1]).read_bytes()[:8192])
else:
    assert sys.argv[1] == "-f" and sys.argv[4] == "/$LogFile"
    image = Path(sys.argv[2])
    image.write_bytes(Path(sys.argv[3]).read_bytes() + image.read_bytes()[8192:])
    if os.environ.get("CHANGE_ORIGINAL"):
        Path(os.environ["CHANGE_ORIGINAL"]).write_bytes(b"changed")
'''
        for tool in ("ntfs-3g.probe", "ntfscat", "ntfscp"):
            executable = self.root / tool
            executable.write_text(stub)
            executable.chmod(0o755)

    def prepare(self):
        return subprocess.run(["bash", str(SCRIPT), str(self.source), str(self.destination)],
                              env=self.environment, capture_output=True, text=True)

    def test_source_is_preserved_and_both_hashes_bind_the_prepared_copy(self):
        result = self.prepare()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.source.read_bytes(), self.before)
        self.assertEqual(self.destination.read_bytes(), b"\xff" * 8192 + b"user file bytes")
        receipt = json.loads(Path(f"{self.destination}.json").read_text())
        self.assertEqual(receipt["source_sha256"], hashlib.sha256(self.before).hexdigest())
        self.assertEqual(receipt["lab_source_sha256"], hashlib.sha256(self.destination.read_bytes()).hexdigest())
        self.assertEqual(receipt["source_case"], "cli-write-at-win-verify-chkdsk")

    def test_dirty_or_unmountable_source_is_refused(self):
        self.environment["PROBE_STATUS"] = "15"
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.source.read_bytes(), self.before)

    def test_existing_image_is_never_overwritten(self):
        self.destination.write_bytes(b"keep")
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertEqual(self.destination.read_bytes(), b"keep")

    def test_dirty_flag_refused_by_ntfscat_cannot_be_hidden_by_a_clean_probe(self):
        self.environment["NTFSCAT_STATUS"] = "1"
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.source.read_bytes(), self.before)

    def test_existing_receipt_is_never_overwritten(self):
        receipt = Path(f"{self.destination}.json")
        receipt.write_bytes(b"keep")
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertEqual(receipt.read_bytes(), b"keep")
        self.assertFalse(self.destination.exists())

    def test_changed_original_cannot_publish_a_lab_source(self):
        self.environment["CHANGE_ORIGINAL"] = str(self.source)
        result = self.prepare()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("changed its original source", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_short_log_is_refused(self):
        self.source.write_bytes(b"RSTR")
        self.assertNotEqual(self.prepare().returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_windows_grade_must_be_unique_clean_and_bound_to_the_image(self):
        self.assertEqual(self.prepare().returncode, 0)
        digest = hashlib.sha256(self.destination.read_bytes()).hexdigest()
        row = ["slate-write-lab-source", "lab", self.destination.name, digest, "clean", "0"]
        variants = [([row], True), ([], False), ([row, row], False)]
        for column, value in [(2, "other.img"), (3, "0" * 64), (4, "not-clean"), (5, "3")]:
            changed = row.copy()
            changed[column] = value
            variants.append(([changed], False))
        for index, (rows, accepted) in enumerate(variants):
            table = self.root / f"grades-{index}.tsv"
            with table.open("w", newline="") as handle:
                writer = csv.writer(handle, delimiter="\t")
                writer.writerow(["case", "origin", "image", "sha256", "windows_verdict", "windows_exit"])
                writer.writerows(rows)
            result = subprocess.run(["bash", str(SCRIPT), "verify", str(table), str(self.destination)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode == 0, accepted, result.stderr)


result = unittest.TextTestRunner(verbosity=0).run(unittest.defaultTestLoader.loadTestsFromTestCase(LabSourceTests))
sys.exit(not result.wasSuccessful())
PY
