### Fixed

- **A fragmented write no longer stops when its mapping pairs outgrow their
  attribute (#1240).** The non-resident attribute now grows into the room the
  record has, moving the attributes after it. Only a record with no room left
  still refuses.
