### Added

- **An incomplete rescue export leaves the NTFS volume dirty.** When
  `fs_ntfs::rescue::export_with` writes an image with missing spans, it sets
  `VOLUME_IS_DIRTY` in the image's `$Volume` `$VOLUME_INFORMATION`, mirrored
  into `$MFTMirr`, so Windows checks the volume before trusting it. The
  report's `dirty_flag` and the error manifest say whether the flag was set;
  a `$Volume` record lost to a missing span is never fabricated, and the
  manifest says why the flag is not set. A complete export never touches the
  flag.
