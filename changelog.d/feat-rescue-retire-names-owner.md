### Changed

- **A cluster in use is left alone, not refused, when retiring clusters, and
  its owning file is named.** `fs_ntfs::repair_plan::plan_retire_clusters_io`
  still retires every free cluster it is asked for; a cluster `$Bitmap` marks
  in use is no longer an error for the whole plan but is reported in
  `RetirePlan::in_use` with the MFT record number and path of the file whose
  attribute maps it, so an unreadable sector inside a file is traced to that
  file instead of being cross-linked into `$BadClus`.
