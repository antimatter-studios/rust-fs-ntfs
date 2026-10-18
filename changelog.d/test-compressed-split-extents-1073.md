### Added

- A test proves a compressed stream whose run list is split into extents
  named in the `$ATTRIBUTE_LIST` is read through every extent: raw,
  LZNT1-compressed and deallocated units, including a unit whose hole
  crosses an extent boundary, by whole and ranged reads (#1073).
