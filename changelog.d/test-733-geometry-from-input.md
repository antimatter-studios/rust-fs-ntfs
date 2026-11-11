### Fixed

- **The formatter writes the 512-byte MFT records its planner accepts.**
  `format_with_plan` failed with "system record 9 too small" for every
  plan with 512-byte records. `$Secure`'s streams now move to extension
  records through an attribute list, and `$Extend` and the root directory
  index into allocated blocks when their entries do not fit a record
  (#1295).
- **`$Extend`'s children survive a mirror of more than sixteen records.**
  With 1024-byte records at 32 KiB or 64 KiB clusters, and 512-byte
  records at 16 KiB clusters or more, the formatter overwrote
  `$ObjId`, `$Reparse` and `$Quota` with free records (#1297).
