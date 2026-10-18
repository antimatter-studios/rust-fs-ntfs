### Fixed

- **A file or directory created on a volume that tracks quotas is charged
  to an owner (#848, part of #676).** `create_file` and `mkdir` left a new
  record's `owner_id` and `quota_charged` at 0, so on a volume with quota
  tracking on it was charged to nobody and its owner's usage in
  `$Extend\$Quota:$Q` did not count it. A record created in a directory
  charged to an owner now takes that owner, is charged its MFT record as
  Windows charges a new file, and the owner's `$Q` usage grows by the
  charge. The owner is the parent directory's until it follows the new
  file's security descriptor (#849).
