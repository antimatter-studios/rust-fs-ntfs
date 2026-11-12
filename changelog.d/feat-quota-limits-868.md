### Added

- **`fs_ntfs::quota` reads `$Extend\$Quota:$Q`.** `entries_io` returns each
  owner's entry, with its usage, warning limit, hard limit and exceeded time,
  and the quota control entry, whose `tracking_enabled` and
  `enforcement_enabled` say whether the volume tracks and enforces quotas.
