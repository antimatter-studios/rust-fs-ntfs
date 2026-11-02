### Fixed

- **Rewriting, truncating or extending a file on a volume that tracks
  quotas moves its charge (#675, part of #510).** With quota tracking on,
  Windows charges each file its MFT record plus the allocated size of its
  non-resident `$DATA`, and keeps each owner's usage in
  `$Extend\$Quota:$Q`. `write_file_contents`, `write_at` and `truncate`
  changed a charged file's allocation and left both alone, so the owner's
  usage drifted from what its files held. Each now sets the file's
  `quota_charged` to its record and data allocation, and moves its owner's
  `$Q` usage by the difference, in the index root or in an index block. A
  file with an `$ATTRIBUTE_LIST` keeps its charge for now.
