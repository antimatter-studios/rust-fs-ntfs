### Added

- **A whole rewrite of the unnamed `$DATA` of a file that has an
  `$ATTRIBUTE_LIST` goes through the list (#788, part of #663).** A file
  Windows made with its unnamed `$DATA` in an extension record was refused
  with "unnamed $DATA attribute not found", and the stream of a listed file
  was only ever written in the base record. `write_file_contents` now writes
  it where the list keeps it, resident while it fits a record and otherwise
  in new clusters, moving it only when its record cannot hold it; a base
  record too full for even the stream's non-resident form gains a list.
