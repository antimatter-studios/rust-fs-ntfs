### Added

- `fs.ntfs TARGET export PATH DEST [--replace]` copies an uncompressed regular
  file through core's shared bounded transfer, with SHA-256 reporting, stdout,
  existing-name refusal and complete-file publication (#832). Directories,
  reparse points and transformed streams are refused; recursive export is
  tracked separately in #833.

### Fixed

- New files and directories reuse a parent's defined nonzero security ID
  instead of dropping its catalog reference. An undefined nonzero parent ID
  is refused before allocating a record.
