### Fixed

- **A change to a directory whose index root is in an extension record is
  refused for the attribute list.** ntfs-3g leaves a large directory's
  `$INDEX_ROOT` in an extension record behind an `$ATTRIBUTE_LIST`. The
  readers follow the list (#471), but creating, unlinking or renaming in
  such a directory failed with "no $INDEX_ROOT on parent". It is now
  refused before anything is written, saying the record carries an
  `$ATTRIBUTE_LIST` this crate does not maintain, as every other change to
  such a record is (#468).
