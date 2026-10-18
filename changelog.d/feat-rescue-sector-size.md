### Changed

- **The rescue scan works in the source's own sector size.** A failed bulk
  read is retried in the bytes per sector the NTFS boot sector names, 4096
  included, so an unreadable span is a whole number of the volume's sectors,
  and the archive records that size. A caller-given sector size that
  disagrees with the boot sector is refused, as is resuming an archive
  against a source of another sector size. `ScanOptions::sector_size` is now
  an `Option`, `None` by default.
