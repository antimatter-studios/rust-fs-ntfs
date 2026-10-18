### Added

- **Changing the EAs of a file whose `$EA` is split keeps its extents
  true (#1058, part of #805).** A non-resident `$EA` whose run list was
  split into extents across records could not be changed: the driver
  refused an attribute split across records. The `$EA` is now rewritten
  whole, as a split named stream is: every old extent's clusters are
  freed, the extents no longer needed go with their list entries,
  extension records left empty are freed, and the new `$EA` is split again
  when its run list needs it. The 64 KiB cap on a file's EAs is unchanged.
