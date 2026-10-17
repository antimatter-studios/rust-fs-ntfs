### Added

- **Growing, partly writing and truncating a split unnamed `$DATA` keep its
  extents true (#800, part of #664).** A stream whose run list was split
  into extents across records could not be rewritten, written at an offset
  or truncated: the driver refused an attribute split across records. The
  stream is now rewritten whole: every old extent's clusters are freed,
  the extents no longer needed go with their list entries, extension
  records left empty are freed, and the new stream is split again when its
  run list needs it.
