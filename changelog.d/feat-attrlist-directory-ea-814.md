### Added

- **An EA a directory's base record cannot hold goes through an
  `$ATTRIBUTE_LIST` (#814, part of #666).** It was refused with "exceeds
  record capacity", and a directory that already had a list refused it with
  "is a directory, whose attributes this driver does not move between
  records". A directory now takes the list as a file does: `$EA` and
  `$EA_INFORMATION` go to an extension record, its `$INDEX_ROOT` and
  `$FILE_NAME` stay in the base record, and names added and removed
  afterwards leave the list true. Removing such a directory frees its
  extension records and the clusters they map with its base record.
