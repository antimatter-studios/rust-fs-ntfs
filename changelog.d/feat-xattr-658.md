### Added

- `fs.ntfs TARGET xattr PATH` lists native NTFS extended attributes with
  stored names, value lengths and NEED_EA flags. Adding NAME reads its raw
  bytes to stdout or `-o FILE`; inspection opens the source read-only (#658).
