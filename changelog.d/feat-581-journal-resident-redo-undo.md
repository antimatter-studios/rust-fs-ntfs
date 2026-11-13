### Added

- **A resident metadata change can be recorded in `$LogFile` as NTFS
  records it (part of #581).** `logfile_write::append_resident_update`
  appends an `UpdateResidentValue` record, holding the new bytes and the
  old, and the `ForgetTransaction` that ends its transaction, after the
  last record of an LFS 2.x log, in memory, and returns the LSN the changed
  MFT record must carry. Both records are encoded exactly as Windows
  encodes them. The write path does not use it yet, and it refuses an LFS
  1.x log, a log marked clean, a log in which `$MFT`'s `$DATA` is not open,
  a record that crosses a log page and a log that would wrap.
