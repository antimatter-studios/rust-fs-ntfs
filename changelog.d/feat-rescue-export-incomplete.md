### Added

- **An incomplete rescue archive exports on explicit request.**
  `fs_ntfs::rescue::export_with` with `ExportOptions { allow_incomplete:
  true }` exports an archive that lacks some source bytes, writing every
  missing span as zeros and checking it reads back as zeros. An error
  manifest beside the image (`export_manifest_path`) lists each span by
  byte offset and length, and lands before the image does. The report names
  the spans and `is_complete()` is false. Without the request the archive is
  refused as before, and an existing manifest is refused like an existing
  image.
