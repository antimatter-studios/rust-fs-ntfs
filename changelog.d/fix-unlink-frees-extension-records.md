### Fixed

- **Unlinking a file's last name frees its extension records and their
  clusters (#565).** When ntfs-3g or Windows had spread a file's attributes
  over extension records behind an `$ATTRIBUTE_LIST`, unlinking it retired
  only the base record and freed only the clusters the base record mapped.
  The extension records stayed in use with no base, and their attributes'
  clusters stayed allocated, which chkdsk reports as orphaned segments.
  Every record the list names is now checked, retired and freed with the
  base record, and every cluster any of them maps is freed; a list naming
  a free record or another file's is refused before anything is written.
