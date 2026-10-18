### Fixed

- **A reparse point kept in an extension record is read back (#812, part
  of #665).** Since a reparse point can be written through a file's
  `$ATTRIBUTE_LIST`, it can live in an extension record, but
  `read_reparse_point_io` looked only in the base record and reported none.
  A file with a list now has its `$REPARSE_POINT` found through the list.
