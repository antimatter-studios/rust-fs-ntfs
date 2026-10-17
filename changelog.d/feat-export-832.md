### Added

- `fs.ntfs TARGET export PATH DEST [--replace]` copies an uncompressed regular
  file through core's shared bounded transfer, with SHA-256 reporting, stdout,
  existing-name refusal and complete-file publication (#832). Directories,
  reparse points and transformed streams are refused; recursive export is
  tracked separately in #833.
