"""Whether a Windows fsutil record is bound to this image and this run.

Library-free on purpose: the acceptance rule runs as a pure test before any
shared library or image is built, and the native consumer imports this same
function, so the controls and the real comparison cannot drift apart.
"""

import hashlib
import re


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def binding_errors(fields, image_path, expected_head):
    """Names of the record fields that do not bind it to this image and run."""
    errors = []
    if fields.get("image sha256") != sha256(image_path):
        errors.append("image sha256")
    head = fields.get("git HEAD", "")
    if not (
        re.fullmatch(r"[0-9a-f]{40}", expected_head or "") and head == expected_head
    ):
        errors.append("git HEAD")
    return errors
