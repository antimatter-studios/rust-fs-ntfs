### Added

- **A read-only rescue scan archives what a failing source can still give.**
  `fs_ntfs::rescue::scan` and `scan_to_path` copy every readable byte of a
  source into a new, append-only external archive of CRC-32-checked records.
  A bulk read that fails with a media error is retried one sector at a time,
  so a bad sector costs only itself, and every unreadable span is recorded
  explicitly with no payload rather than filled in. The source is never
  written, an existing file is never taken as the archive, and each record is
  flushed before the scan reads past it. The layout is in
  `docs/rescue-archive.md`.
