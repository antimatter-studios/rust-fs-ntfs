### Added

- **`fs.ntfs mkdir --parents PATH` (`-p`) makes every missing directory on
  the way to PATH.** A directory already at PATH, or at any step, is
  success, and the report lists only what was made:
  `{"path": ..., "created": [...]}`. Names are found as Windows finds them,
  without regard to case, and `.` and `..` are read from the text of PATH as
  Windows reads them. A file or a symbolic link on the way or at PATH, a
  name Windows cannot address and a dirty volume are refused on a read-only
  open, with the whole chain rehearsed through the library's `mkdir`, so a
  refused command makes no part of the chain and leaves the image
  byte-identical. Windows lists every directory made, spelled exactly so,
  refuses the chains a file blocks as `fs.ntfs` does, and finds the volume
  clean with `chkdsk` (#647).
