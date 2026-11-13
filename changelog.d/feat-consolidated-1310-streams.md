### Fixed

- **`fs.ntfs write --at` and `fs.ntfs truncate` accept a file whose unnamed
  `$DATA` lives only in an extension record (#1328).** A base record full
  of named streams and EAs moves the whole `$DATA` attribute out when it
  goes non-resident. The driver writes and resizes such a stream through
  the `$ATTRIBUTE_LIST`, but both verbs read the attribute's flags from the
  base record alone and refused the file with "no unnamed $DATA in the base
  record". They now read the flags from the record the list names for the
  stream's first extent.
- **A write into a file with an `$ATTRIBUTE_LIST` no longer needs free
  space the size of the whole file (#1347).** A write at an offset, a whole
  rewrite or a truncate of an unnamed `$DATA` reached through the list
  copied the stream into new clusters before it freed the old ones, so
  patching a few bytes of a large, fragmented file failed with "no N free
  clusters" on a volume with less than the file's size free. The stream is
  now rewritten in the clusters it already has; only the clusters it grows
  by are allocated, and those past a shorter end are freed.
- **`fs.ntfs write` replaces a file whose unnamed `$DATA` lives only in an
  extension record (#1347).** The whole-file replacement looked for the
  stream in the base record alone and refused such a file with "has no
  unnamed $DATA". A file with an `$ATTRIBUTE_LIST` is now replaced through
  the list, as the library's whole rewrite already was.
