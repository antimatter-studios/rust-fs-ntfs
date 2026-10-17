### Added

- **`fs.ntfs ls --recursive PATH` (`-R`) walks a tree within stated bounds.**
  Entries add the volume-root path and depth, in deterministic byte-sorted
  depth-first order. Symlinks and junctions are listed without following
  any reparse point, hard links retain every name, and repeated directory
  records are marked `revisited`. The default depth is 64 and entry limit
  100000; exact fits succeed, while overflow or an unreadable directory
  keeps the partial report and exits 1. Windows supplies the independent
  recursive enumeration and file identities (#644).
