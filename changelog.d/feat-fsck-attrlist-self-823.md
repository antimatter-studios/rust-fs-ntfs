### Added

- `fsck::check_io` reports an `$ATTRIBUTE_LIST` that lists itself, naming
  the base file record and entry offset even when the entry identifies the
  list attribute correctly. Checking remains read-only (#1040, part of #823).
