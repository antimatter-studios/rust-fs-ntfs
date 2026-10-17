### Added

- **A partial write to the unnamed `$DATA` of a file that has an
  `$ATTRIBUTE_LIST` goes through the list (#789, part of #663).**
  `write_at` on a file Windows made with its unnamed `$DATA` in an
  extension record was refused with "unnamed $DATA attribute not found",
  and on any listed file it edited the base record alone. It now reads the
  stream through the list and writes it where the list keeps it, into the
  middle or past the end, resident or in clusters, moving it only when its
  record cannot hold it.
