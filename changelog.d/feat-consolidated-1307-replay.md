### Fixed

- Interrupted-capture classification distinguishes a refused replay from a replay that leaves damage, reports its redo count and remaining findings, and preserves the classification artifact when grading fails (#1231).
- A `$LogFile` replay that rewrites `$Volume`'s own record keeps the volume's dirty flag set. The log puts back the flags Windows logged, so an in-place repair of a capture whose log touched `$Volume` (the `freed` capture) cleared the flag before its audit; `fsck` now clears it only when asked (#519, #716).
