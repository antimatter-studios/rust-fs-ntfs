### Fixed

- **A write whose quota recharge cannot succeed is refused before any of
  its data lands (#1081, part of #847).** On a volume that tracks quotas,
  `write_file_contents`, `write_at` and `truncate` moved a charged file's
  charge only after its data was written, so a file whose owner had no
  `$Extend\$Quota:$Q` entry, or was charged less than the file, got an
  error back with its data already changed. The owner's `$Q` entry is now
  checked first, and such a write is refused with the file and `$Q`
  exactly as they were.
