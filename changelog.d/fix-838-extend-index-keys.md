### Fixed

- **A freshly formatted volume's `$Extend` index agrees with its children (#838).**
  `mkfs` now keys `$Extend`'s `$I30` entries for `$ObjId`, `$Reparse` and
  `$Quota` with each child's own `$FILE_NAME` times, sizes and attributes, as
  Windows `format` writes them. The zeroed keys it wrote before were accepted
  by Windows `chkdsk` but reported by Slate as `index-duplicate-information`.
