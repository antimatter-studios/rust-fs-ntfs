## Interfaces

| Slate copy-only writer step (test support) | Experimental: `scripts/slate-oracle.sh write` runs Slate's `ntfs-write-lab` on a read-only copy of each source image, keeps each written image and records both SHA-256 values; a changed source, a failed write or an unchanged copy fails by case name. Wiring the step into the advisory workflow with real write cases, Windows grading and our reader's comparison remain #863, #864 and #865 | Unreleased | #863, #690, #580 | `tests/scripts/slate-oracle.sh` |
