### Added

- `fsck::check_io` reports every extension attribute that its base record's
`$ATTRIBUTE_LIST` does not name by record reference, type, exact UTF-16 name,
lowest VCN and instance, including extensions omitted from the list entirely.
Findings identify the base record, extension record and attribute without
writing to the volume (#822, part of #667).
