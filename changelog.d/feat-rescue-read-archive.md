### Added

- **A rescue archive reads back to its last whole record.**
  `fs_ntfs::rescue::read_archive` checks the header and every record's
  CRC-32 and returns the whole records with where each starts in the archive.
  A record an interruption cut short at the end of the file is left out and
  its bytes reported, so a resumed scan knows where to append; a whole record
  with a bad checksum, kind, class or length is refused as damage, naming its
  archive byte.
