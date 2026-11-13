### Fixed

- **A `$LogFile` record that wraps onto the first page of the record area is
  read from that page** (#654). The replay took the first page found at its
  own home for the start of the record area. When a record wrapped past the
  log's end and left only its continuation on the first page, that page
  named the log's last page, so the continuation was read from the page
  after it, and the records written there were read as dirty page table
  entries: `fsck -y` refused the volume with "a dirty page of no clusters".
  The record area now starts after the two restart pages and the 32
  tail-copy pages.
