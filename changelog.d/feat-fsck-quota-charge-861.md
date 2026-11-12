### Added

- **The checker reports a file whose quota charge drifted.** On a volume
  tracking quotas, `check_io` (and `fs.ntfs fsck`) compares each charged
  file's own charge in `$STANDARD_INFORMATION` with what its allocation
  costs (its MFT record plus each non-resident `$DATA` stream's allocated
  size), and reports every file that differs as `QuotaChargeDrift`
  (`quota_charge_drift` in the CLI's JSON), naming the record and both
  numbers. Directories, files with an `$ATTRIBUTE_LIST`, and sparse or
  compressed streams are not compared yet.
