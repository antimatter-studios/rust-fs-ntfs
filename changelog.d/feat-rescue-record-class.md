### Changed

- **Rescue archive records name the class of space their span was in.** The
  archive layout is now version 2: each record header carries a class
  (unclassified, NTFS metadata, allocated, free, or the tail past the last
  cluster) before its CRC-32, and `ScanReport::class` counts archived and
  unreadable bytes per class. A linear scan marks every record unclassified.
  The layout is in `docs/rescue-archive.md`.
