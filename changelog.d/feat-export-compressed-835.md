### Added

- `fs.ntfs export` copies an LZNT1-compressed file, decompressing one
  compression unit at a time so memory stays bounded by one unit plus the
  transfer's chunk; ranged reads of a compressed stream no longer
  decompress the whole stream (#835). Encrypted (EFS) streams stay refused.
