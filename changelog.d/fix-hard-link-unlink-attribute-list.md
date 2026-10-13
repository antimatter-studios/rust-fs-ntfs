### Fixed

- **Unlinking one of a file's names no longer breaks its attribute list.**
  A file with several names, whose record carries an `$ATTRIBUTE_LIST`
  (ntfs-3g and Windows add one once the names outgrow the record), lost the
  `$FILE_NAME` from its base record while the list still named it, and the
  unlink returned `Ok`. It is now refused before anything is written, as
  adding or removing any other attribute of such a record already was
  (#468).
