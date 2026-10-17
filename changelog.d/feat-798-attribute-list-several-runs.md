### Added

- **A write through a file's `$ATTRIBUTE_LIST` takes several runs when no
  free run holds the stream (#798, part of #664).** The unnamed `$DATA`
  and the named streams of a listed file went into clusters as one run, so
  on fragmented free space, where every free run was shorter than the
  stream, the write was refused with "no contiguous free run" however much
  space the volume had. As Windows does, the stream is now placed in as
  many runs as it takes, while its mapping pairs still fit one MFT record.
