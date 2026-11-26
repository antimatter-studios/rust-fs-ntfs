### Changed

- Writable mounts admit 4Kn volumes with 1 KiB FILE records, whose update
  sequence arrays use the verified 512-byte stride. Windows' `chkdsk`,
  listing and hashes accept the files and directories created, written,
  renamed and removed on such a volume. Other volumes
  with records smaller than their sectors are still read only.

### Fixed

- Removing or renaming the last name of a directory index leaf no longer
  leaves the leaf empty in the index, which Windows' `chkdsk` reports as an
  error in the directory.
