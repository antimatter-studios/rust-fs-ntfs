### Changed

- Writable mounts admit 4Kn volumes with 1 KiB FILE records, whose update
  sequence arrays use the verified 512-byte stride. Windows' `chkdsk`,
  listing and hashes accept the files and directories created, written,
  renamed and removed on such a volume.
