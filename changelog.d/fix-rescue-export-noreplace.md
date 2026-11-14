### Fixed

- **A rescue export never replaces a destination created after its check.**
  `fs_ntfs::rescue::export_to_path` moves its finished image into place with
  `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS,
  or a hard link where neither is available, instead of a rename that
  replaces whatever is at the destination by then. A file created there
  during the export is left byte-for-byte unchanged, the export fails naming
  the destination, and no `.partial` file is left behind; where none of
  those moves works the export fails rather than replacing. The error
  manifest beside an incomplete export moves into place the same way.
