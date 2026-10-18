### Added

- **Resuming a rescue archive refuses a source whose retained bytes changed.**
  `fs_ntfs::rescue::resume` reads each archived data record's span from the
  source again before it cuts or appends anything. A sector that still reads
  and differs refuses the resume, naming that sector's offset, with the
  archive unchanged; another NTFS volume is refused the same way. A sector
  that no longer reads is not a difference.
