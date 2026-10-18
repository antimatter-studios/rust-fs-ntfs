### Added

- **Rewriting a split named stream keeps its extents true (#805, part of
  #664).** A named stream whose run list was split into extents across
  records could not be rewritten: the driver refused an attribute split
  across records. The stream is now rewritten whole, as a split unnamed
  `$DATA` is: every old extent's clusters are freed, the extents no longer
  needed go with their list entries, extension records left empty are
  freed, and the new stream is split again when its run list needs it.
