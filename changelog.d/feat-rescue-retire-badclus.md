### Added

- **Free clusters can be retired into `$BadClus`.**
  `fs_ntfs::repair_plan::plan_retire_clusters_io` plans the writes that map
  each free cluster at its own LCN in `$BadClus:$Bad`, merged with the runs
  already there, and mark it in use in `$Bitmap`; nothing is written to the
  volume it is planned from, so copy-repair applies it to a new target. A
  cluster `$BadClus` already maps plans nothing, and a cluster in use is
  refused, since mapping it would cross-link the attribute that owns it.
