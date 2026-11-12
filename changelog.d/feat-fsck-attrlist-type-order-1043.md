### Added

- `fsck::check_io` reports descending attribute type codes in an
  `$ATTRIBUTE_LIST`, naming the base file record, offending entry offset,
  and both type codes. Checking remains read-only (#1043, part of #1041).
