### Added

- **`fs.ntfs mv SOURCE PATH` renames a name within its directory.** PATH is
  the new name itself, through the library's rename: the same length,
  longer, shorter or case only, for a file or a directory. A move to another
  directory answers `not implemented` (exit 3) rather than being attempted.
  A name already at PATH, compared as Windows compares names, is refused,
  never replaced. Every refusal is decided on a read-only open, with the
  rename rehearsed in full, so a refused command leaves the image
  byte-identical. Windows reads every renamed file back under its new name
  and `chkdsk` finds the volume clean (#645).
