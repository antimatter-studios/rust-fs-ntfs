### Added

- **A run list too long for one MFT record is split into extents named in
  the `$ATTRIBUTE_LIST` (#799, part of #664).** A stream written through a
  file's `$ATTRIBUTE_LIST` onto fragmented free space takes a run per hole,
  and once its mapping pairs outgrew one MFT record the write was refused.
  As Windows does, the run list is now split into extents with ascending
  starting VCNs, each in an extension record with its own lowest and
  highest VCN and its own list entry, the first carrying the stream's
  sizes.
