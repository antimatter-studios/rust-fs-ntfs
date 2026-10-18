### Added

- **Named streams on a full directory record use its `$ATTRIBUTE_LIST`
  (#815, part of #666).** Streams spill into extension records while the
  directory's index stays in its base record. The directory support added
  for EAs also enables this path; regression checks now require every
  stream to read back through the independent `ntfs` crate and the
  directory's entries to remain intact.
