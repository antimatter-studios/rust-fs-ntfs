### Added

- **A complete rescue archive exports to a new image, and an incomplete one
  is refused.** `fs_ntfs::rescue::export_to_path` refuses an archive with any
  source byte no data record holds, naming the first missing span, before
  anything is created. Otherwise it writes a temporary file beside the
  destination, flushes it, reads every data record's span back, and only
  then renames it into place, reporting the image's byte count and SHA-256.
  An existing destination is refused, and the archive is only read.
