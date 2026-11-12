### Added

- **A reparse point on a directory goes through its `$ATTRIBUTE_LIST`
  (#1212).** Setting or removing one on a directory that already has a
  list, or setting one too large for the directory's base record, now
  takes the list path a file takes, with the directory's index left in its
  base record. Both were refused before.
