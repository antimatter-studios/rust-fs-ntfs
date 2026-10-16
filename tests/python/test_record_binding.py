"""The fsutil record's binding rule, exercised without a library or image.

These controls call the same binding_errors the Windows comparison uses, over
synthetic file bytes and records, so they run before any ABI or volume setup
and a defect in the rule fails here first.
"""

import hashlib
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
from record_binding import binding_errors

HEAD = "0123456789abcdef0123456789abcdef01234567"
OTHER_HEAD = "fedcba9876543210fedcba9876543210fedcba98"


class RecordBinding(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.image = os.path.join(self.dir.name, "image.bin")
        self.other = os.path.join(self.dir.name, "other.bin")
        with open(self.image, "wb") as f:
            f.write(b"synthetic image bytes")
        with open(self.other, "wb") as f:
            f.write(b"different synthetic bytes")
        digest = hashlib.sha256(b"synthetic image bytes").hexdigest()
        self.fields = {"image sha256": digest, "git HEAD": HEAD}

    def tearDown(self):
        self.dir.cleanup()

    def test_matching_image_and_head_are_accepted(self):
        self.assertEqual(binding_errors(self.fields, self.image, HEAD), [])

    def test_a_valid_but_different_head_is_refused(self):
        self.assertIn("git HEAD", binding_errors(self.fields, self.image, OTHER_HEAD))

    def test_a_mismatched_image_is_refused(self):
        self.assertIn("image sha256", binding_errors(self.fields, self.other, HEAD))

    def test_a_record_without_a_head_is_refused(self):
        fields = {"image sha256": self.fields["image sha256"]}
        self.assertIn("git HEAD", binding_errors(fields, self.image, HEAD))

    def test_a_missing_expected_head_is_refused(self):
        self.assertIn("git HEAD", binding_errors(self.fields, self.image, ""))


if __name__ == "__main__":
    unittest.main()
