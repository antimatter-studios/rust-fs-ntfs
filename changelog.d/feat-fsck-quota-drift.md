### Added

- **The checker reports quota accounting drift.** On a volume tracking
  quotas, `check_io` (and `fs.ntfs fsck`) sums what each owner's files are
  charged in their `$STANDARD_INFORMATION` and compares it with the owner's
  usage in `$Extend\$Quota:$Q`, reporting every owner whose usage differs as
  `QuotaUsageDrift` (`quota_usage_drift` in the CLI's JSON), with the owner
  id and both numbers. A volume with tracking off is not checked, and a
  `$Q` that cannot be read is reported as `QuotaUnreadable`.
