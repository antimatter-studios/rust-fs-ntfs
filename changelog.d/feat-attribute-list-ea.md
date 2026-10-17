### Added

- **An EA that its file's base record cannot hold, or one written to a file
  that has an `$ATTRIBUTE_LIST`, goes through the list, as Windows does
  (#660, part of #480).** An EA on a full 1 KiB record was refused with
  "exceeds record capacity", and any EA on a file with a list was refused
  outright. The file is now given a list when it has none, and `$EA` and
  `$EA_INFORMATION` are written, rewritten and removed wherever the list
  puts them; listing a file's EAs follows the list. An EA set too large for
  any record is still refused, and so are the EAs of a directory whose
  record cannot hold them.
- **`rust-ntfs ea` sets or removes one of a file's extended attributes.**

### Fixed

- On-disk `$EA` entries retain their aligned length in the last entry too,
  matching Windows-authored bytes instead of copying the API buffer's zero
  terminator; chkdsk reported corrupt EA information after attribute-list EA
  writes (#745).
