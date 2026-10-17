### Added

- **Unlinking one of several names of a file that has an `$ATTRIBUTE_LIST`
  takes the name out through the list (#778, part of #662).** It was refused
  with "unlink a hard link: the record carries an $ATTRIBUTE_LIST". The
  `$FILE_NAME` is now found in whichever record holds it, base or extension,
  as Windows lays them out, and removed with its list entry; an extension
  record it leaves empty is freed, and the base record counts one link fewer.
