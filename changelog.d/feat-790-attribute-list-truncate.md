### Added

- **A truncate of the unnamed `$DATA` of a file that has an
  `$ATTRIBUTE_LIST` goes through the list (#790, part of #663).**
  `truncate` on a file Windows made with its unnamed `$DATA` in an
  extension record was refused with "unnamed $DATA attribute not found",
  and on any listed file it resized the stream in the base record alone.
  It now reads the stream through the list, cuts it short or extends it
  with zeros, and writes it where the list keeps it, resident or in
  clusters, freeing the clusters past the new end once nothing names them.
