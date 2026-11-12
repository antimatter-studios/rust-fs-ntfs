### Added

- **The checker reports a hard-link count its file's names do not make
  (#583).** `fsck::check_io` and `fsck.ntfs` now compare each base
  record's hard-link count with the `$FILE_NAME` attributes it holds,
  every one but a DOS short name, and report a `LinkCountDrift`
  (`link_count_drift` in the JSON report) when they differ. A file with
  an `$ATTRIBUTE_LIST`, whose names may sit in extension records, is not
  compared yet.
