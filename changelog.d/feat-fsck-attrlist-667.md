### Added

- **The checker reports an `$ATTRIBUTE_LIST` entry naming a record that is
  not the file's extension.** `fsck::check_io` now reads every base
  record's list and reports a `BadAttributeList` (`bad_attribute_list` in
  `fs.ntfs fsck --json`) for an entry naming a record `$MFT`'s bitmap marks
  free, whose header is not in use, whose sequence number is not the
  entry's, or whose base reference names another file (#820).
