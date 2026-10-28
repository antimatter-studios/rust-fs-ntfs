### Added

- **An image's change journal reads offline (#609).** `usn_image::journal_state_io`
  reports whether a volume has a change journal -- absent, being deleted, or
  active -- and an active journal's ID, `FirstUsn`, `NextUsn`,
  `LowestValidUsn`, maximum size and allocation delta from `$UsnJrnl:$Max`
  and `$J`, with no Windows API. `usn_image::read_records_io` reads a bounded
  batch of `USN_RECORD_V2` records from a saved cursor and returns the
  cursor of the first record it did not read; a cursor the journal can no
  longer answer is refused with the reason, never answered with an empty
  batch.
